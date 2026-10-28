//! Safe extraction.
//!
//! Every format handler feeds entries through the same extraction visitor, which owns
//! all of the security policy:
//!
//! * entry names are sanitised — `..` is rejected, absolute paths and drive
//!   prefixes are stripped, and nothing can resolve outside the destination;
//! * no write ever goes through a symbolic link (each existing path
//!   component is checked with `symlink_metadata`);
//! * symbolic links are created last, only if their target stays inside the
//!   destination, and are re-verified after all links exist;
//! * byte counts are measured while decoding (declared sizes are not trusted)
//!   and checked against [`ExtractLimits`];
//! * files are written to a temporary file and renamed into place, so a
//!   failure never leaves a half-written file under the final name;
//! * existing files are never replaced unless [`OverwritePolicy`] allows it.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::archive::{Entry, EntryKind, EntryVisitor, OperationSummary, ReadContext, resolve_format};
use crate::encryption::Password;
use crate::error::{IoContext, OxideError, Result};
use crate::formats::ArchiveFormat;
use crate::handlers;
use crate::progress::{Operation, Progress, Tracker};

/// What to do when an extracted file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverwritePolicy {
    /// Stop with [`OxideError::AlreadyExists`].
    #[default]
    Error,
    /// Leave the existing file alone and continue.
    Skip,
    /// Replace the existing file.
    Overwrite,
    /// Write alongside it as `name (1).ext`, `name (2).ext`, ...
    Rename,
}

/// How symbolic links stored in an archive are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SymlinkPolicy {
    /// Create links whose target resolves inside the destination; skip
    /// (with a warning) any that would point outside it.
    #[default]
    Contained,
    /// Never create links.
    Skip,
}

/// Size of the reusable copy buffer used while extracting.
const COPY_BUFFER: usize = 256 * 1024;

/// Files up to this size are written by the background writer pool.
const POOLED_FILE_MAX: u64 = 1024 * 1024;
/// Writer threads. File creation stops scaling at about four on APFS.
const MAX_WRITERS: usize = 4;

/// Resource limits that protect against decompression bombs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExtractLimits {
    /// Maximum bytes written in total.
    pub max_total_bytes: Option<u64>,
    /// Maximum bytes for any single entry.
    pub max_entry_bytes: Option<u64>,
    /// Maximum number of entries processed.
    pub max_entries: Option<u64>,
    /// Maximum ratio of extracted bytes to archive size. Only enforced
    /// once more than [`ExtractLimits::RATIO_GRACE_BYTES`] have been written.
    pub max_ratio: Option<u64>,
}

impl ExtractLimits {
    /// Output below this size is never rejected for its ratio alone.
    pub const RATIO_GRACE_BYTES: u64 = 64 * 1024 * 1024;

    /// No limits at all. Only use for archives you trust.
    pub fn unlimited() -> Self {
        ExtractLimits { max_total_bytes: None, max_entry_bytes: None, max_entries: None, max_ratio: None }
    }
}

impl Default for ExtractLimits {
    /// 64 GiB total, 1 million entries, 1000:1 ratio.
    fn default() -> Self {
        ExtractLimits {
            max_total_bytes: Some(64 * 1024 * 1024 * 1024),
            max_entry_bytes: None,
            max_entries: Some(1_000_000),
            max_ratio: Some(1000),
        }
    }
}

/// Settings for [`extract`].
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Behaviour for files that already exist.
    pub overwrite: OverwritePolicy,
    /// Behaviour for symbolic links.
    pub symlinks: SymlinkPolicy,
    /// Bomb protection.
    pub limits: ExtractLimits,
    /// Only extract these archive paths (and, for directories, everything
    /// beneath them). `None` extracts everything.
    pub selection: Option<Vec<String>>,
    /// Password for encrypted archives.
    pub password: Option<Password>,
    /// Skip detection and treat the file as this format.
    pub format: Option<ArchiveFormat>,
    /// Apply stored permission bits (Unix only; setuid/setgid/sticky are always dropped).
    pub preserve_permissions: bool,
    /// Apply stored modification times.
    pub preserve_mtime: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            overwrite: OverwritePolicy::Error,
            symlinks: SymlinkPolicy::Contained,
            limits: ExtractLimits::default(),
            selection: None,
            password: None,
            format: None,
            preserve_permissions: true,
            preserve_mtime: true,
        }
    }
}

/// Extracts an archive into `destination`, creating it if necessary.
///
/// On error, files already completed remain on disk, but no partially
/// written file is ever left under its final name.
pub fn extract(
    archive: &Path,
    destination: &Path,
    options: &ExtractOptions,
    progress: &Progress,
) -> Result<OperationSummary> {
    let format = resolve_format(archive, options.format)?;
    let handler = handlers::handler_for(format)?;
    fs::create_dir_all(destination).at(destination)?;
    let root = fs::canonicalize(destination).at(destination)?;

    let tracker = Tracker::new(progress, Operation::Extract);
    let ctx = ReadContext { path: archive, format, password: options.password.as_ref(), tracker: &tracker };
    let archive_len = ctx.archive_len();
    tracker.set_input_total(archive_len);

    let mut visitor = ExtractVisitor::new(root, options, &tracker, archive_len);
    let result = handler.read(&ctx, &mut visitor).and_then(|()| visitor.finish());
    if progress.cancel_token().is_cancelled() {
        return Err(OxideError::Cancelled);
    }
    result?;
    tracker.finish();
    let mut summary = visitor.summary;
    summary.elapsed = tracker.elapsed();
    Ok(summary)
}

/// Turns an archive entry name into a safe relative path.
///
/// Returns `Ok(None)` for names that refer to the destination itself
/// (e.g. `./`). Backslashes are treated as separators, leading `/` and
/// Windows drive or UNC prefixes are removed, and any `..` component is an
/// error because it could only be used to climb out of the destination.
pub fn sanitize_entry_path(raw: &str) -> Result<Option<PathBuf>> {
    let unsafe_path = |reason| OxideError::UnsafePath { path: raw.to_owned(), reason };
    if raw.contains('\0') {
        return Err(unsafe_path("contains a NUL byte"));
    }
    let normalized = raw.replace('\\', "/");
    let mut out = PathBuf::new();
    let mut first = true;
    for part in normalized.split('/') {
        let is_first = std::mem::replace(&mut first, false);
        match part {
            "" | "." => continue,
            ".." => return Err(unsafe_path("contains a parent-directory (..) component")),
            // Strip drive prefixes such as `C:` that make a path absolute on Windows.
            p if is_first && p.len() == 2 && p.ends_with(':') && p.as_bytes()[0].is_ascii_alphabetic() => continue,
            p => {
                let clean = sanitize_component(p);
                if clean.is_empty() {
                    continue;
                }
                out.push(clean);
            }
        }
    }
    // Defence in depth: the result must consist solely of normal components.
    if out.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(unsafe_path("resolves to a non-relative path"));
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Makes a single path component valid on the current platform.
fn sanitize_component(part: &str) -> String {
    #[cfg(windows)]
    {
        const RESERVED: &[&str] = &[
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
            "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let mut s: String =
            part.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
        // Trailing dots and spaces are silently dropped by Windows, which
        // could make two different entries collide.
        if s.ends_with('.') || s.ends_with(' ') {
            s.pop();
            s.push('_');
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            s.insert(0, '_');
        }
        s
    }
    #[cfg(not(windows))]
    {
        part.to_owned()
    }
}

/// Lexically resolves `target` relative to the directory containing the
/// link at `link`, returning the result relative to the destination root,
/// or `None` if it would leave the root or is absolute.
fn resolve_link_target(link: &Path, target: &str) -> Option<PathBuf> {
    let target = target.replace('\\', "/");
    if target.starts_with('/') || target.contains('\0') {
        return None;
    }
    if target.len() >= 2 && target.as_bytes()[1] == b':' {
        return None;
    }
    let mut stack: Vec<String> = link
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            p => stack.push(p.to_owned()),
        }
    }
    Some(stack.iter().collect())
}

struct PendingLink {
    rel: PathBuf,
    target: String,
    raw: String,
}

/// The visitor that writes entries to disk under the destination root.
pub(crate) struct ExtractVisitor<'a> {
    root: PathBuf,
    options: &'a ExtractOptions,
    tracker: &'a Tracker<'a>,
    archive_len: u64,
    selection: Option<Vec<String>>,
    entries_seen: u64,
    total_written: u64,
    written: HashSet<PathBuf>,
    /// Directories (relative to the root) already verified or created, so
    /// thousands of files in one folder don't re-check every parent.
    /// Safe to cache: links are only created after all files are written.
    checked_dirs: HashSet<PathBuf>,
    /// Copy buffer reused for every file.
    buf: Vec<u8>,
    /// Background writers for small files, started on first use.
    pool: Option<WriterPool>,
    /// Final paths handed to the writer pool that may not exist on disk yet,
    /// keyed by [`name_key`] so case-only clashes are seen on macOS/Windows.
    reserved: HashSet<PathBuf>,
    links: Vec<PendingLink>,
    dir_times: Vec<(PathBuf, SystemTime)>,
    stripped_absolute: bool,
    pub(crate) summary: OperationSummary,
}

impl<'a> ExtractVisitor<'a> {
    pub(crate) fn new(root: PathBuf, options: &'a ExtractOptions, tracker: &'a Tracker<'a>, archive_len: u64) -> Self {
        let selection = options
            .selection
            .as_ref()
            .map(|sel| sel.iter().map(|s| s.replace('\\', "/").trim_matches('/').to_owned()).collect());
        ExtractVisitor {
            root,
            options,
            tracker,
            archive_len,
            selection,
            entries_seen: 0,
            total_written: 0,
            written: HashSet::new(),
            checked_dirs: HashSet::new(),
            buf: vec![0u8; COPY_BUFFER],
            pool: None,
            reserved: HashSet::new(),
            links: Vec::new(),
            dir_times: Vec::new(),
            stripped_absolute: false,
            summary: OperationSummary::default(),
        }
    }

    fn selected(&self, entry: &Entry) -> bool {
        let Some(sel) = &self.selection else {
            return true;
        };
        let path = entry.path.trim_matches('/');
        sel.iter().any(|s| {
            s.is_empty()
                || path == s
                || (path.len() > s.len() && path.starts_with(s.as_str()) && path.as_bytes()[s.len()] == b'/')
        })
    }

    fn check_declared_size(&self, entry: &Entry) -> Result<()> {
        let Some(size) = entry.size else {
            return Ok(());
        };
        let limits = &self.options.limits;
        if let Some(max) = limits.max_entry_bytes
            && size > max
        {
            return Err(OxideError::LimitExceeded(format!(
                "{} declares {size} bytes, above the per-entry limit of {max}",
                entry.path
            )));
        }
        if let Some(max) = limits.max_total_bytes
            && self.total_written.saturating_add(size) > max
        {
            return Err(OxideError::LimitExceeded(format!("total output would exceed {max} bytes")));
        }
        Ok(())
    }

    fn check_written(&self, entry_bytes: u64, name: &str) -> Result<()> {
        let limits = &self.options.limits;
        if let Some(max) = limits.max_entry_bytes
            && entry_bytes > max
        {
            return Err(OxideError::LimitExceeded(format!("{name} exceeds the per-entry limit of {max} bytes")));
        }
        if let Some(max) = limits.max_total_bytes
            && self.total_written > max
        {
            return Err(OxideError::LimitExceeded(format!("total output exceeds {max} bytes")));
        }
        if let Some(ratio) = limits.max_ratio
            && self.total_written > ExtractLimits::RATIO_GRACE_BYTES
            && self.total_written / self.archive_len.max(1) > ratio
        {
            return Err(OxideError::LimitExceeded(format!(
                "compression ratio exceeds {ratio}:1 (possible decompression bomb)"
            )));
        }
        Ok(())
    }

    /// Fails if any existing component of `rel` (under the root) is a symbolic
    /// link. Unlike [`Self::ensure_dir`], creates nothing.
    fn check_no_symlinks(&self, rel: &Path) -> Result<()> {
        let mut current = self.root.clone();
        for component in rel.components() {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(OxideError::UnsafePath {
                        path: rel.display().to_string(),
                        reason: "a path component is a symbolic link",
                    });
                }
                Ok(_) => {}
                Err(_) => return Ok(()),
            }
        }
        Ok(())
    }

    /// Creates every directory along `rel` under the root, refusing to
    /// traverse an existing symbolic link or to replace a non-directory.
    fn ensure_dir(&mut self, rel: &Path) -> Result<PathBuf> {
        if self.checked_dirs.contains(rel) {
            return Ok(self.root.join(rel));
        }
        let mut current = self.root.clone();
        let mut so_far = PathBuf::new();
        for component in rel.components() {
            current.push(component);
            so_far.push(component);
            if self.checked_dirs.contains(&so_far) {
                continue;
            }
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(OxideError::UnsafePath {
                        path: rel.display().to_string(),
                        reason: "a parent directory is a symbolic link",
                    });
                }
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(OxideError::io(
                        &current,
                        io::Error::new(io::ErrorKind::AlreadyExists, "a file exists where a directory is needed"),
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists && current.is_dir() => {}
                    Err(e) => return Err(OxideError::io(&current, e)),
                },
                Err(e) => return Err(OxideError::io(&current, e)),
            }
            self.checked_dirs.insert(so_far.clone());
        }
        Ok(current)
    }

    /// Decides the final path for a new file according to the overwrite
    /// policy. Returns `None` if the entry should be skipped.
    fn resolve_target(&mut self, rel: &Path) -> Result<Option<(PathBuf, bool)>> {
        let target = self.root.join(rel);
        let reserved = &self.reserved;
        let taken = |p: &Path| reserved.contains(&name_key(p)) || fs::symlink_metadata(p).is_ok();
        let exists = match fs::symlink_metadata(&target) {
            Ok(meta) => {
                if meta.is_dir() {
                    return Err(OxideError::io(
                        &target,
                        io::Error::new(io::ErrorKind::AlreadyExists, "a directory exists with this name"),
                    ));
                }
                true
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.reserved.contains(&name_key(&target)),
            Err(e) => return Err(OxideError::io(&target, e)),
        };
        // An archive may legitimately contain the same path twice (e.g. an
        // appended tar); the later copy wins over what we wrote ourselves.
        if !exists || self.written.contains(rel) {
            return Ok(Some((target, exists)));
        }
        match self.options.overwrite {
            OverwritePolicy::Error => Err(OxideError::AlreadyExists(target)),
            OverwritePolicy::Skip => Ok(None),
            OverwritePolicy::Overwrite => Ok(Some((target, true))),
            OverwritePolicy::Rename => Ok(Some((unique_name(&target, taken), false))),
        }
    }

    fn write_file(&mut self, entry: &Entry, rel: &Path, data: &mut dyn Read) -> Result<()> {
        // A later copy of a path must not race an earlier one still being written.
        if self.written.contains(rel) {
            self.flush_writes()?;
        }
        let parent_rel = rel.parent().unwrap_or(Path::new(""));
        let parent = self.ensure_dir(parent_rel)?;
        let Some((target, replace)) = self.resolve_target(rel)? else {
            self.summary.skipped += 1;
            return Ok(());
        };
        let meta = FileMeta {
            mode: entry.mode.filter(|_| self.options.preserve_permissions),
            mtime: entry.modified.filter(|_| self.options.preserve_mtime).and_then(unix_to_system_time),
        };

        self.reserved.insert(name_key(&target));
        let mut buf = std::mem::take(&mut self.buf);
        let result = self.write_contents(entry, &parent, &target, replace, meta, data, &mut buf);
        self.buf = buf;
        let entry_bytes = result?;
        self.written.insert(rel.to_path_buf());
        self.summary.files += 1;
        self.summary.bytes += entry_bytes;
        Ok(())
    }

    /// Small files are read into memory and written by the writer pool (file
    /// creation, not decoding, dominates for them); anything larger, or that
    /// turns out larger than declared, is streamed to disk here.
    #[allow(clippy::too_many_arguments)]
    fn write_contents(
        &mut self,
        entry: &Entry,
        parent: &Path,
        target: &Path,
        replace: bool,
        meta: FileMeta,
        data: &mut dyn Read,
        buf: &mut [u8],
    ) -> Result<u64> {
        let mut head = Vec::new();
        if entry.size.is_some_and(|s| s <= POOLED_FILE_MAX) {
            let mut entry_bytes = 0u64;
            loop {
                let n = self.read_chunk(entry, data, buf, &mut entry_bytes)?;
                if n == 0 {
                    let rename_on_clash = self.options.overwrite == OverwritePolicy::Rename;
                    self.pool().submit(WriteJob {
                        parent: parent.to_path_buf(),
                        target: target.to_path_buf(),
                        replace,
                        rename_on_clash,
                        data: head,
                        meta,
                    })?;
                    return Ok(entry_bytes);
                }
                head.extend_from_slice(&buf[..n]);
                if head.len() as u64 > POOLED_FILE_MAX {
                    break; // larger than declared: fall back to streaming
                }
            }
        }

        let mut tmp = tempfile::Builder::new().prefix(".oxide-").suffix(".part").tempfile_in(parent).at(parent)?;
        let mut entry_bytes = head.len() as u64;
        {
            // Some decoders (ZIP's) return small chunks; batch them into large writes.
            let mut out = io::BufWriter::with_capacity(COPY_BUFFER, tmp.as_file_mut());
            out.write_all(&head).at(target)?;
            loop {
                let n = self.read_chunk(entry, data, buf, &mut entry_bytes)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).at(target)?;
            }
            out.flush().at(target)?;
        }
        finalize(tmp, target, replace, meta)?;
        Ok(entry_bytes)
    }

    /// Reads one chunk, enforcing limits and reporting progress.
    fn read_chunk(
        &mut self,
        entry: &Entry,
        data: &mut dyn Read,
        buf: &mut [u8],
        entry_bytes: &mut u64,
    ) -> Result<usize> {
        loop {
            self.tracker.check_cancel()?;
            let n = match data.read(buf) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(self.decode_error(e, entry)),
            };
            *entry_bytes += n as u64;
            self.total_written += n as u64;
            self.check_written(*entry_bytes, &entry.path)?;
            self.tracker.add_bytes(n as u64);
            return Ok(n);
        }
    }

    fn pool(&mut self) -> &mut WriterPool {
        self.pool.get_or_insert_with(WriterPool::new)
    }

    /// Waits for queued writes and reports the first failure, if any.
    fn flush_writes(&mut self) -> Result<()> {
        match &mut self.pool {
            Some(pool) => pool.flush(),
            None => Ok(()),
        }
    }

    fn decode_error(&self, err: io::Error, entry: &Entry) -> OxideError {
        if self.tracker.cancel().is_cancelled() {
            return OxideError::Cancelled;
        }
        if entry.encrypted && err.kind() == io::ErrorKind::InvalidData && self.options.password.is_some() {
            return OxideError::WrongPassword;
        }
        match OxideError::from_decode(err) {
            OxideError::Corrupt(msg) => OxideError::Corrupt(format!("{}: {msg}", entry.path)),
            other => other,
        }
    }

    fn write_hardlink(&mut self, entry: &Entry, rel: &Path) -> Result<()> {
        // The link's target may still be queued in the writer pool.
        self.flush_writes()?;
        let raw_target = entry.link_target.as_deref().unwrap_or_default();
        let Some(target_rel) = sanitize_entry_path(raw_target)? else {
            return Err(OxideError::UnsafePath { path: entry.path.clone(), reason: "hard link has an empty target" });
        };
        let source = self.root.join(&target_rel);
        // The source must be a regular file we can reach without symlinks.
        self.check_no_symlinks(&target_rel)?;
        match fs::symlink_metadata(&source) {
            Ok(m) if m.is_file() => {}
            _ => {
                self.summary
                    .warnings
                    .push(format!("skipped hard link {} (target {raw_target} was not extracted)", entry.path));
                self.summary.skipped += 1;
                return Ok(());
            }
        }
        self.ensure_dir(rel.parent().unwrap_or(Path::new("")))?;
        let Some((target, replace)) = self.resolve_target(rel)? else {
            self.summary.skipped += 1;
            return Ok(());
        };
        if replace {
            fs::remove_file(&target).at(&target)?;
        }
        if fs::hard_link(&source, &target).is_err() {
            // Filesystems without hard links: fall back to a copy.
            fs::copy(&source, &target).at(&target)?;
        }
        self.written.insert(rel.to_path_buf());
        self.summary.links += 1;
        Ok(())
    }

    /// Creates deferred symbolic links and verifies none escapes the root.
    pub(crate) fn finish(&mut self) -> Result<()> {
        self.flush_writes()?;
        let links = std::mem::take(&mut self.links);
        let mut created = Vec::new();
        for link in links {
            self.tracker.check_cancel()?;
            if resolve_link_target(&link.rel, &link.target).is_none() {
                self.summary.warnings.push(format!(
                    "skipped symbolic link {} -> {} (points outside the destination)",
                    link.raw, link.target
                ));
                self.summary.skipped += 1;
                continue;
            }
            self.ensure_dir(link.rel.parent().unwrap_or(Path::new("")))?;
            let Some((path, replace)) = self.resolve_target(&link.rel)? else {
                self.summary.skipped += 1;
                continue;
            };
            if replace {
                fs::remove_file(&path).at(&path)?;
            }
            match make_symlink(&link.target, &path) {
                Ok(()) => {
                    self.summary.links += 1;
                    created.push(path);
                }
                Err(e) => {
                    self.summary.warnings.push(format!("could not create symbolic link {}: {e}", link.raw));
                    self.summary.skipped += 1;
                }
            }
        }
        // Each link was checked lexically; a chain of links could still
        // resolve elsewhere, so verify the real resolution now that all exist.
        for path in created {
            if let Ok(real) = fs::canonicalize(&path)
                && !real.starts_with(&self.root)
            {
                let _ = fs::remove_file(&path);
                return Err(OxideError::UnsafePath {
                    path: path.display().to_string(),
                    reason: "symbolic link chain resolves outside the destination",
                });
            }
        }
        // Directory mtimes last, since creating children updates them.
        if self.options.preserve_mtime {
            for (dir, t) in self.dir_times.drain(..).rev() {
                if let Ok(f) = File::open(&dir) {
                    let _ = f.set_modified(t);
                }
            }
        }
        if self.stripped_absolute {
            self.summary.warnings.push("removed leading '/' or drive letters from absolute entry paths".into());
        }
        Ok(())
    }
}

impl EntryVisitor for ExtractVisitor<'_> {
    fn begin(&mut self, index: Option<&[Entry]>) {
        if let Some(index) = index {
            let selected: Vec<_> = index.iter().filter(|e| self.selected(e)).collect();
            let bytes = selected.iter().filter_map(|e| e.size).sum();
            self.tracker.set_totals(Some(selected.len() as u64), Some(bytes));
        }
    }

    fn wants(&mut self, entry: &Entry) -> Result<bool> {
        if !self.selected(entry) {
            return Ok(false);
        }
        self.entries_seen += 1;
        if let Some(max) = self.options.limits.max_entries
            && self.entries_seen > max
        {
            return Err(OxideError::LimitExceeded(format!("archive has more than {max} entries")));
        }
        self.check_declared_size(entry)?;
        Ok(true)
    }

    fn visit(&mut self, entry: &Entry, data: Option<&mut dyn Read>) -> Result<()> {
        self.tracker.check_cancel()?;
        let Some(rel) = sanitize_entry_path(&entry.path)? else {
            return Ok(());
        };
        // Matches what `sanitize_entry_path` strips: a leading separator or a
        // first component that is exactly a drive letter like `C:`.
        let first = entry.path.split(['/', '\\']).next().unwrap_or_default();
        let drive = first.len() == 2 && first.ends_with(':') && first.as_bytes()[0].is_ascii_alphabetic();
        if entry.path.starts_with(['/', '\\']) || drive {
            self.stripped_absolute = true;
        }
        self.tracker.start_entry(&entry.path);
        match entry.kind {
            EntryKind::Directory => {
                let dir = self.ensure_dir(&rel)?;
                if let Some(t) = entry.modified.and_then(unix_to_system_time) {
                    self.dir_times.push((dir, t));
                }
                self.summary.directories += 1;
            }
            EntryKind::File => {
                let mut empty = io::empty();
                let data: &mut dyn Read = match data {
                    Some(d) => d,
                    None => &mut empty,
                };
                self.write_file(entry, &rel, data)?;
            }
            EntryKind::Symlink => match self.options.symlinks {
                SymlinkPolicy::Skip => self.summary.skipped += 1,
                SymlinkPolicy::Contained => self.links.push(PendingLink {
                    rel,
                    target: entry.link_target.clone().unwrap_or_default(),
                    raw: entry.path.clone(),
                }),
            },
            EntryKind::Hardlink => self.write_hardlink(entry, &rel)?,
            EntryKind::Other => {
                self.summary.warnings.push(format!("skipped special file {}", entry.path));
                self.summary.skipped += 1;
            }
        }
        self.tracker.finish_entry();
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FileMeta {
    mode: Option<u32>,
    mtime: Option<SystemTime>,
}

/// Applies metadata and moves a finished temporary file to its final name.
fn finalize(tmp: tempfile::NamedTempFile, target: &Path, replace: bool, meta: FileMeta) -> Result<()> {
    let file = tmp.as_file();
    #[cfg(unix)]
    if let Some(mode) = meta.mode {
        use std::os::unix::fs::PermissionsExt;
        // Never restore setuid/setgid/sticky bits from untrusted input.
        let _ = file.set_permissions(fs::Permissions::from_mode(mode & 0o777));
    }
    if let Some(t) = meta.mtime {
        let _ = file.set_modified(t);
    }
    let persisted = if replace { tmp.persist(target) } else { tmp.persist_noclobber(target) };
    persisted.map_err(|e| match e.error.kind() {
        io::ErrorKind::AlreadyExists => OxideError::AlreadyExists(target.to_path_buf()),
        _ => OxideError::io(target, e.error),
    })?;
    Ok(())
}

/// A file fully decided on by the extractor (path checked, policy applied),
/// waiting to be written.
struct WriteJob {
    parent: PathBuf,
    target: PathBuf,
    replace: bool,
    /// Under the rename policy, pick another free name if the target turns up taken.
    rename_on_clash: bool,
    data: Vec<u8>,
    meta: FileMeta,
}

impl WriteJob {
    fn run(self) -> Result<()> {
        let mut tmp =
            tempfile::Builder::new().prefix(".oxide-").suffix(".part").tempfile_in(&self.parent).at(&self.parent)?;
        tmp.as_file_mut().write_all(&self.data).at(&self.target)?;
        match finalize(tmp, &self.target, self.replace, self.meta) {
            Err(OxideError::AlreadyExists(_)) if self.rename_on_clash => {
                let target = unique_name(&self.target, |p| fs::symlink_metadata(p).is_ok());
                WriteJob { target, rename_on_clash: false, ..self }.run()
            }
            other => other,
        }
    }
}

/// A few threads that create small files concurrently. Creating a file costs
/// far more than writing a few kilobytes, and filesystems handle several
/// creations at once much faster than one after another.
struct WriterPool {
    jobs: Option<mpsc::SyncSender<WriteJob>>,
    results: mpsc::Receiver<Result<()>>,
    workers: Vec<std::thread::JoinHandle<()>>,
    pending: usize,
    failed: Option<OxideError>,
}

impl WriterPool {
    fn new() -> Self {
        let count = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, MAX_WRITERS);
        // A short queue bounds memory to a few megabytes of pending files.
        let (job_tx, job_rx) = mpsc::sync_channel::<WriteJob>(count * 2);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (result_tx, results) = mpsc::channel();
        let workers = (0..count)
            .filter_map(|i| {
                let (jobs, results) = (job_rx.clone(), result_tx.clone());
                std::thread::Builder::new()
                    .name(format!("oxide-writer-{i}"))
                    .spawn(move || {
                        loop {
                            let job = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv();
                            let Ok(job) = job else { return };
                            if results.send(job.run()).is_err() {
                                return;
                            }
                        }
                    })
                    .ok()
            })
            .collect();
        WriterPool { jobs: Some(job_tx), results, workers, pending: 0, failed: None }
    }

    fn submit(&mut self, job: WriteJob) -> Result<()> {
        self.collect(false);
        if let Some(e) = self.failed.take() {
            return Err(e);
        }
        if self.workers.is_empty() {
            return job.run(); // no threads could be started: write inline
        }
        match &self.jobs {
            Some(tx) if tx.send(job).is_ok() => {
                self.pending += 1;
                Ok(())
            }
            _ => Err(OxideError::from(io::Error::other("file writer threads stopped"))),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.collect(true);
        self.failed.take().map_or(Ok(()), Err)
    }

    /// Gathers finished results; with `wait`, until nothing is pending.
    fn collect(&mut self, wait: bool) {
        while self.pending > 0 {
            let result = if wait {
                match self.results.recv() {
                    Ok(r) => r,
                    Err(_) => break,
                }
            } else {
                match self.results.try_recv() {
                    Ok(r) => r,
                    Err(_) => break,
                }
            };
            self.pending -= 1;
            if let Err(e) = result
                && self.failed.is_none()
            {
                self.failed = Some(e);
            }
        }
    }
}

impl Drop for WriterPool {
    fn drop(&mut self) {
        // Closing the queue lets workers finish what was handed to them and exit.
        self.jobs = None;
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Key for comparing file names the way the platform's default filesystem
/// does: case-insensitively on macOS and Windows.
fn name_key(path: &Path) -> PathBuf {
    if cfg!(any(target_os = "macos", windows)) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

/// `name (1).ext`, `name (2).ext`, ... — the first one `taken` rejects.
fn unique_name(target: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = target.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1.. {
        let candidate = target.with_file_name(format!("{stem} ({n}){ext}"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("ran out of candidate file names")
}

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &str, link: &Path) -> io::Result<()> {
    // Creating links needs Developer Mode or elevation; failures become warnings.
    let target = target.replace('/', "\\");
    let resolved = link.parent().map(|p| p.join(&target));
    if resolved.is_some_and(|p| p.is_dir()) {
        std::os::windows::fs::symlink_dir(&target, link)
    } else {
        std::os::windows::fs::symlink_file(&target, link)
    }
}

#[cfg(not(any(unix, windows)))]
fn make_symlink(_target: &str, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "symbolic links are not supported on this platform"))
}

pub(crate) fn unix_to_system_time(secs: i64) -> Option<SystemTime> {
    if secs >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    }
}

pub(crate) fn system_time_to_unix(t: SystemTime) -> Option<i64> {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(e) => i64::try_from(e.duration().as_secs()).ok().map(|s| -s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str) -> String {
        sanitize_entry_path(raw).unwrap().unwrap().to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn sanitizes_normal_and_absolute_paths() {
        assert_eq!(ok("a/b/c.txt"), "a/b/c.txt");
        assert_eq!(ok("/etc/passwd"), "etc/passwd");
        assert_eq!(ok("./a//b/"), "a/b");
        assert_eq!(ok("C:\\Windows\\x.dll"), "Windows/x.dll");
        assert_eq!(ok("\\\\server\\share\\f"), "server/share/f");
        assert!(sanitize_entry_path("./").unwrap().is_none());
    }

    #[test]
    fn rejects_traversal() {
        for bad in ["../x", "a/../../x", "a\\..\\..\\x", "..", "a/..", "x\0y"] {
            assert!(matches!(sanitize_entry_path(bad), Err(OxideError::UnsafePath { .. })), "{bad} should be rejected");
        }
    }

    #[test]
    fn link_targets_are_contained() {
        assert_eq!(resolve_link_target(Path::new("a/link"), "b.txt"), Some(PathBuf::from("a/b.txt")));
        assert_eq!(resolve_link_target(Path::new("a/link"), "../b.txt"), Some(PathBuf::from("b.txt")));
        assert_eq!(resolve_link_target(Path::new("a/link"), "../../b.txt"), None);
        assert_eq!(resolve_link_target(Path::new("link"), "/etc/passwd"), None);
        assert_eq!(resolve_link_target(Path::new("link"), "C:/x"), None);
    }

    #[test]
    fn time_round_trip() {
        for s in [0i64, 1_700_000_000, -86_400] {
            assert_eq!(system_time_to_unix(unix_to_system_time(s).unwrap()), Some(s));
        }
    }
}
