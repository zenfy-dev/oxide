//! Archive metadata types, the format-handler abstraction, and the
//! read-only operations (`list`, `test`) plus in-place editing (`add`).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::compression::{CreateJob, CreateOptions};
use crate::encryption::Password;
use crate::error::{IoContext, OxideError, Result};
use crate::formats::{self, ArchiveFormat};
use crate::handlers;
use crate::progress::{Operation, Progress, Tracker};

/// The kind of filesystem object an entry represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link; see [`Entry::link_target`].
    Symlink,
    /// Hard link to an earlier entry; see [`Entry::link_target`].
    Hardlink,
    /// Device nodes, FIFOs and other special files. Never extracted.
    Other,
}

/// One item stored in an archive.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Path inside the archive, `/`-separated, exactly as stored apart from
    /// backslashes being normalised. Not yet validated for safety.
    pub path: String,
    /// What the entry is.
    pub kind: EntryKind,
    /// Uncompressed size, if recorded.
    pub size: Option<u64>,
    /// Compressed size, if the format records it per entry.
    pub compressed_size: Option<u64>,
    /// Modification time as Unix seconds.
    pub modified: Option<i64>,
    /// Unix permission bits, if recorded.
    pub mode: Option<u32>,
    /// Whether the entry's data is encrypted.
    pub encrypted: bool,
    /// Target of a symbolic or hard link.
    pub link_target: Option<String>,
}

impl Entry {
    pub(crate) fn new(path: impl Into<String>, kind: EntryKind) -> Self {
        Entry {
            path: path.into().replace('\\', "/"),
            kind,
            size: None,
            compressed_size: None,
            modified: None,
            mode: None,
            encrypted: false,
            link_target: None,
        }
    }

    /// The path with any trailing `/` removed.
    pub fn trimmed_path(&self) -> &str {
        self.path.trim_end_matches('/')
    }

    /// The final path component.
    pub fn name(&self) -> &str {
        let trimmed = self.trimmed_path();
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }

    /// True for directories.
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// The result of listing an archive.
#[derive(Debug, Clone, Serialize)]
pub struct ArchiveInfo {
    /// Where the archive lives.
    pub path: PathBuf,
    /// Detected format.
    pub format: ArchiveFormat,
    /// Size of the archive file in bytes.
    pub archive_size: u64,
    /// Every entry, in archive order.
    pub entries: Vec<Entry>,
}

impl ArchiveInfo {
    /// Number of non-directory entries.
    pub fn file_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_dir()).count()
    }

    /// Number of directory entries.
    pub fn dir_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_dir()).count()
    }

    /// Sum of known uncompressed sizes.
    pub fn total_size(&self) -> u64 {
        self.entries.iter().filter_map(|e| e.size).sum()
    }

    /// True if any entry is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.entries.iter().any(|e| e.encrypted)
    }

    /// Archive size divided by uncompressed size (lower is better), when meaningful.
    pub fn compression_ratio(&self) -> Option<f64> {
        let total = self.total_size();
        (total > 0).then(|| self.archive_size as f64 / total as f64)
    }
}

/// Options shared by the read-only operations.
#[derive(Debug, Clone, Default)]
pub struct ReadOptions {
    /// Password for encrypted archives.
    pub password: Option<Password>,
    /// Skip detection and treat the file as this format.
    pub format: Option<ArchiveFormat>,
}

/// Counters describing a finished operation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct OperationSummary {
    /// Regular files written, archived, or verified.
    pub files: u64,
    /// Directories created or archived.
    pub directories: u64,
    /// Links created or archived.
    pub links: u64,
    /// Uncompressed bytes processed.
    pub bytes: u64,
    /// Entries skipped (existing files, unselected entries are not counted).
    pub skipped: u64,
    /// Non-fatal problems worth showing to the user.
    pub warnings: Vec<String>,
    /// Wall-clock duration.
    #[serde(serialize_with = "serialize_secs")]
    pub elapsed: Duration,
}

fn serialize_secs<S: serde::Serializer>(d: &Duration, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

/// Everything a format handler needs to read an archive.
pub(crate) struct ReadContext<'a> {
    pub path: &'a Path,
    pub format: ArchiveFormat,
    pub password: Option<&'a Password>,
    /// Progress, cancellation, and compressed-input accounting.
    pub tracker: &'a Tracker<'a>,
}

impl ReadContext<'_> {
    pub(crate) fn archive_len(&self) -> u64 {
        fs::metadata(self.path).map(|m| m.len()).unwrap_or(0)
    }
}

/// Receives entries as a handler walks an archive.
///
/// Extraction, testing and listing are all visitors over the same
/// handler code, so every format gets identical safety checks.
pub(crate) trait EntryVisitor {
    /// Called once before the first entry. Random-access formats pass the
    /// full index so totals can be computed; streaming formats pass `None`.
    fn begin(&mut self, _index: Option<&[Entry]>) {}

    /// Whether this entry's data should be decoded and passed to [`visit`].
    /// May fail early, e.g. when a declared size already breaks a limit.
    fn wants(&mut self, _entry: &Entry) -> Result<bool> {
        Ok(true)
    }

    /// When false the handler may verify data internally (checksums) and
    /// call [`visit`] with `None` for files. Used by RAR testing.
    #[cfg_attr(not(feature = "rar"), allow(dead_code))]
    fn needs_data(&self) -> bool {
        true
    }

    /// Handles one entry. `data` is the decoded content for regular files.
    fn visit(&mut self, entry: &Entry, data: Option<&mut dyn Read>) -> Result<()>;
}

/// The per-format implementation behind every public operation.
///
/// Adding a format means implementing this trait and registering it in
/// [`handlers::handler_for`]; path safety, limits, overwrite handling and
/// progress are provided by the shared visitor and job types.
pub(crate) trait FormatHandler: Sync {
    /// Reads the entry index. The default walks the archive without decoding data.
    fn list(&self, ctx: &ReadContext) -> Result<Vec<Entry>> {
        struct Collect(Vec<Entry>);
        impl EntryVisitor for Collect {
            fn wants(&mut self, entry: &Entry) -> Result<bool> {
                self.0.push(entry.clone());
                Ok(false)
            }
            fn visit(&mut self, _: &Entry, _: Option<&mut dyn Read>) -> Result<()> {
                Ok(())
            }
        }
        let mut c = Collect(Vec::new());
        self.read(ctx, &mut c)?;
        Ok(c.0)
    }

    /// Walks every entry, decoding data for those the visitor wants.
    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()>;

    /// Writes a new archive containing `job.inputs` to `out`.
    fn create(&self, out: &mut File, job: &CreateJob) -> Result<()>;

    /// Writes a copy of the archive at `ctx.path` plus `job.inputs` to `out`.
    /// Existing entries whose names collide with new inputs are replaced.
    fn append(&self, ctx: &ReadContext, _out: &mut File, _job: &CreateJob) -> Result<()> {
        Err(OxideError::Unsupported { format: ctx.format.name(), operation: "adding files to an existing archive" })
    }
}

pub(crate) fn resolve_format(path: &Path, forced: Option<ArchiveFormat>) -> Result<ArchiveFormat> {
    match forced {
        Some(f) => Ok(f),
        None => formats::detect_format(path),
    }
}

/// Lists an archive's entries without extracting anything.
pub fn list(path: &Path, options: &ReadOptions) -> Result<ArchiveInfo> {
    list_with_progress(path, options, &Progress::none())
}

/// Like [`list`], with progress and cancellation. Useful for compressed
/// TAR files, which have no index and must be decompressed to be listed.
pub fn list_with_progress(path: &Path, options: &ReadOptions, progress: &Progress) -> Result<ArchiveInfo> {
    let format = resolve_format(path, options.format)?;
    let handler = handlers::handler_for(format)?;
    let tracker = Tracker::new(progress, Operation::List);
    let ctx = ReadContext { path, format, password: options.password.as_ref(), tracker: &tracker };
    tracker.set_input_total(ctx.archive_len());
    let result = handler.list(&ctx);
    if progress.cancel_token().is_cancelled() {
        return Err(OxideError::Cancelled);
    }
    let entries = result?;
    tracker.finish();
    Ok(ArchiveInfo { path: path.to_path_buf(), format, archive_size: ctx.archive_len(), entries })
}

/// Verifies an archive by decoding every entry and checking its checksums,
/// without writing anything to disk.
pub fn test(path: &Path, options: &ReadOptions, progress: &Progress) -> Result<OperationSummary> {
    let format = resolve_format(path, options.format)?;
    let handler = handlers::handler_for(format)?;
    let tracker = Tracker::new(progress, Operation::Test);
    let ctx = ReadContext { path, format, password: options.password.as_ref(), tracker: &tracker };
    tracker.set_input_total(ctx.archive_len());
    let mut visitor = TestVisitor { tracker: &tracker, summary: OperationSummary::default() };
    let result = handler.read(&ctx, &mut visitor);
    if progress.cancel_token().is_cancelled() {
        return Err(OxideError::Cancelled);
    }
    result?;
    tracker.finish();
    let mut summary = visitor.summary;
    summary.elapsed = tracker.elapsed();
    Ok(summary)
}

struct TestVisitor<'t, 'p> {
    tracker: &'t Tracker<'p>,
    summary: OperationSummary,
}

impl EntryVisitor for TestVisitor<'_, '_> {
    fn begin(&mut self, index: Option<&[Entry]>) {
        if let Some(index) = index {
            let bytes = index.iter().filter_map(|e| e.size).sum();
            self.tracker.set_totals(Some(index.len() as u64), Some(bytes));
        }
    }

    fn needs_data(&self) -> bool {
        false
    }

    fn visit(&mut self, entry: &Entry, data: Option<&mut dyn Read>) -> Result<()> {
        self.tracker.check_cancel()?;
        self.tracker.start_entry(&entry.path);
        match entry.kind {
            EntryKind::Directory => self.summary.directories += 1,
            EntryKind::Symlink | EntryKind::Hardlink => self.summary.links += 1,
            EntryKind::File | EntryKind::Other => {
                self.summary.files += 1;
                match data {
                    Some(reader) => {
                        let n = drain(reader, self.tracker)?;
                        self.summary.bytes += n;
                    }
                    // The handler verified the data itself (RAR).
                    None => {
                        let n = entry.size.unwrap_or(0);
                        self.tracker.add_bytes(n);
                        self.summary.bytes += n;
                    }
                }
            }
        }
        self.tracker.finish_entry();
        Ok(())
    }
}

/// Reads `reader` to the end, reporting progress and honouring cancellation.
pub(crate) fn drain(reader: &mut dyn Read, tracker: &Tracker) -> Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        tracker.check_cancel()?;
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(OxideError::from_decode(e)),
        };
        total += n as u64;
        tracker.add_bytes(n as u64);
    }
}

/// Adds files to an existing archive.
///
/// The archive is rewritten to a temporary file next to it (existing
/// entries are copied without recompression where the format allows) and
/// atomically swapped in only after the new archive is complete, so a
/// failure never damages the original. Supported for ZIP and the TAR family.
///
/// Settings such as compression level and password in `options` apply to
/// the new entries; `options.format` is ignored.
pub fn add(
    archive: &Path,
    sources: &[PathBuf],
    options: &CreateOptions,
    progress: &Progress,
) -> Result<OperationSummary> {
    let format = formats::detect_format(archive)?;
    let handler = handlers::handler_for(format)?;
    let tracker = Tracker::new(progress, Operation::Create);
    let job = CreateJob::prepare(format, sources, Some(archive), options, &tracker)?;
    let ctx = ReadContext { path: archive, format, password: options.password.as_ref(), tracker: &tracker };
    let parent = parent_dir(archive);
    let mut tmp = tempfile::Builder::new().prefix(".oxide-").suffix(".tmp").tempfile_in(&parent).at(&parent)?;
    let result = handler.append(&ctx, tmp.as_file_mut(), &job);
    if progress.cancel_token().is_cancelled() {
        return Err(OxideError::Cancelled);
    }
    result?;
    tmp.as_file().sync_all().at(tmp.path())?;
    // Preserve the original archive's permissions on the replacement.
    if let Ok(meta) = fs::metadata(archive) {
        let _ = fs::set_permissions(tmp.path(), meta.permissions());
    }
    tmp.persist(archive).map_err(|e| OxideError::io(archive, e.error))?;
    tracker.finish();
    Ok(job.summary(&tracker))
}

pub(crate) fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}
