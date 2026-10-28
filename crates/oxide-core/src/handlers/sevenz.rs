//! 7z support via the pure-Rust `sevenz-rust2` crate.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::time::SystemTime;

use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
use sevenz_rust2::{
    ArchiveEntry, ArchiveReader, ArchiveWriter, EncoderConfiguration, EncoderMethod, Error as SzError,
    Password as SzPassword, SourceReader,
};

use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::{CreateJob, InputItem, InputKind, TrackedReader};
use crate::error::{IoContext, OxideError, Result};
use crate::extraction::system_time_to_unix;
use crate::formats::CompressionMethod;

pub(crate) struct SevenZHandler;

/// Flag set by p7zip/7-Zip when the high 16 attribute bits hold a Unix mode.
const UNIX_EXTENSION: u32 = 0x8000;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_ARCHIVE: u32 = 0x20;
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
/// Upper bound on uncompressed bytes per solid block.
const SOLID_BLOCK_BYTES: u64 = 256 * 1024 * 1024;
const SOLID_BLOCK_FILES: usize = 10_000;
const MAX_LINK_TARGET: u64 = 4096;

fn map_err(err: SzError) -> OxideError {
    match err {
        SzError::PasswordRequired => OxideError::PasswordRequired,
        SzError::MaybeBadPassword(_) => OxideError::WrongPassword,
        SzError::Io(e, _) | SzError::FileOpen(e, _) => OxideError::from_decode(e),
        other => OxideError::Corrupt(other.to_string()),
    }
}

fn unix_mode(e: &ArchiveEntry) -> Option<u32> {
    (e.has_windows_attributes && e.windows_attributes & UNIX_EXTENSION != 0).then_some(e.windows_attributes >> 16)
}

fn entry_of(e: &ArchiveEntry, encrypted: bool) -> Entry {
    let mode = unix_mode(e);
    let kind = if e.is_directory {
        EntryKind::Directory
    } else if mode.is_some_and(|m| m & S_IFMT == S_IFLNK) {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    let mut entry = Entry::new(e.name(), kind);
    entry.size = Some(e.size);
    entry.modified =
        e.has_last_modified_date.then(|| SystemTime::from(e.last_modified_date)).and_then(system_time_to_unix);
    entry.mode = mode.map(|m| m & 0o7777);
    entry.encrypted = encrypted;
    entry
}

fn open(ctx: &ReadContext) -> Result<ArchiveReader<BufReader<File>>> {
    let file = File::open(ctx.path).at(ctx.path)?;
    let password = ctx.password.map(|p| SzPassword::new(p.expose())).unwrap_or_else(SzPassword::empty);
    ArchiveReader::new(BufReader::with_capacity(256 * 1024, file), password).map_err(map_err)
}

fn is_encrypted(reader: &ArchiveReader<BufReader<File>>) -> bool {
    reader
        .archive()
        .blocks
        .iter()
        .any(|b| b.coders.iter().any(|c| c.encoder_method_id() == EncoderMethod::ID_AES256_SHA256))
}

impl FormatHandler for SevenZHandler {
    fn list(&self, ctx: &ReadContext) -> Result<Vec<Entry>> {
        let reader = open(ctx)?;
        let encrypted = is_encrypted(&reader);
        Ok(reader.archive().files.iter().filter(|e| !e.is_anti_item()).map(|e| entry_of(e, encrypted)).collect())
    }

    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()> {
        let mut reader = open(ctx)?;
        let encrypted = is_encrypted(&reader);
        let index: Vec<Entry> =
            reader.archive().files.iter().filter(|e| !e.is_anti_item()).map(|e| entry_of(e, encrypted)).collect();
        visitor.begin(Some(&index));

        // Errors from our side are stashed here because the callback must
        // return the library's error type.
        let mut failure: Option<OxideError> = None;
        let result = reader.for_each_entries(|raw, data| {
            if raw.is_anti_item() {
                return Ok(true);
            }
            let entry = entry_of(raw, encrypted);
            let outcome = (|| -> Result<()> {
                ctx.tracker.check_cancel()?;
                if visitor.wants(&entry)? {
                    match entry.kind {
                        EntryKind::Symlink => {
                            let mut target = String::new();
                            data.take(MAX_LINK_TARGET).read_to_string(&mut target).map_err(OxideError::from_decode)?;
                            let mut link = entry.clone();
                            link.link_target = Some(target);
                            visitor.visit(&link, None)?;
                        }
                        EntryKind::File => visitor.visit(&entry, Some(data))?,
                        _ => visitor.visit(&entry, None)?,
                    }
                }
                // In a solid block the next entry starts where this one ends,
                // so any unread bytes must be consumed.
                io::copy(data, &mut io::sink()).map_err(OxideError::from_decode)?;
                Ok(())
            })();
            match outcome {
                Ok(()) => Ok(true),
                Err(e) => {
                    failure = Some(e);
                    Ok(false)
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        result.map_err(map_err)
    }

    fn create(&self, out: &mut File, job: &CreateJob) -> Result<()> {
        let mut writer = ArchiveWriter::new(BufWriter::with_capacity(1 << 20, out)).map_err(map_err)?;
        let mut methods: Vec<EncoderConfiguration> = Vec::new();
        if let Some(pw) = job.password {
            methods.push(AesEncoderOptions::new(SzPassword::new(pw.expose())).into());
        }
        let level = job.level.unwrap_or(6).clamp(0, 9) as u32;
        methods.push(match job.method {
            CompressionMethod::Store => EncoderConfiguration::new(EncoderMethod::COPY),
            CompressionMethod::Lzma2 if job.threads > 1 => {
                Lzma2Options::from_level_mt(level, job.threads, 16 * 1024 * 1024).into()
            }
            CompressionMethod::Lzma2 => Lzma2Options::from_level(level).into(),
            other => return Err(OxideError::InvalidOption(format!("7z cannot use the {other} method"))),
        });
        writer.set_content_methods(methods);
        writer.set_encrypt_header(job.password.is_some());

        let sz_err = |e: SzError| if job.tracker.cancel().is_cancelled() { OxideError::Cancelled } else { map_err(e) };
        let mut batch: Vec<&InputItem> = Vec::new();
        let mut batch_bytes = 0u64;
        for item in &job.inputs {
            job.tracker.check_cancel()?;
            match &item.kind {
                InputKind::File { size } if *size > 0 => {
                    if !batch.is_empty() && (batch_bytes + size > SOLID_BLOCK_BYTES || batch.len() >= SOLID_BLOCK_FILES)
                    {
                        push_solid(&mut writer, std::mem::take(&mut batch), job).map_err(sz_err)?;
                        batch_bytes = 0;
                    }
                    batch.push(item);
                    batch_bytes += size;
                }
                InputKind::File { .. } | InputKind::Directory => {
                    job.tracker.start_entry(&item.name);
                    writer.push_archive_entry(archive_entry(item), None::<&[u8]>).map_err(sz_err)?;
                    job.tracker.finish_entry();
                }
                InputKind::Symlink { target } => {
                    job.tracker.start_entry(&item.name);
                    let mut entry = archive_entry(item);
                    entry.windows_attributes = UNIX_EXTENSION | ATTR_ARCHIVE | ((S_IFLNK | 0o777) << 16);
                    writer.push_archive_entry(entry, Some(target.as_bytes())).map_err(sz_err)?;
                    job.tracker.finish_entry();
                }
            }
        }
        if !batch.is_empty() {
            push_solid(&mut writer, batch, job).map_err(sz_err)?;
        }
        let mut inner = writer.finish().map_err(|e| job.map_err(e, std::path::Path::new("archive")))?;
        inner.flush()?;
        Ok(())
    }
}

fn archive_entry(item: &InputItem) -> ArchiveEntry {
    let mut entry = match item.kind {
        InputKind::Directory => ArchiveEntry::new_directory(&item.name),
        _ => ArchiveEntry::new_file(&item.name),
    };
    if let Some(t) = item.modified.and_then(|t| sevenz_rust2::NtTime::try_from(t).ok()) {
        entry.last_modified_date = t;
        entry.has_last_modified_date = true;
    }
    let (file_type, dos) = match item.kind {
        InputKind::Directory => (S_IFDIR, ATTR_DIRECTORY),
        _ => (S_IFREG, ATTR_ARCHIVE),
    };
    entry.has_windows_attributes = true;
    entry.windows_attributes = match item.mode {
        Some(mode) => UNIX_EXTENSION | dos | ((file_type | mode) << 16),
        None => dos,
    };
    entry
}

/// Opens its file on first read so a large solid block doesn't hold
/// thousands of file handles at once.
struct LazyInput<'j, 'a> {
    job: &'j CreateJob<'a>,
    item: &'j InputItem,
    reader: Option<TrackedReader<'a, File>>,
}

impl Read for LazyInput<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.reader.is_none() {
            self.job.tracker.start_entry(&self.item.name);
            self.reader = Some(self.job.open(self.item).map_err(|e| io::Error::other(e.to_string()))?);
        }
        let n = self.reader.as_mut().map_or(Ok(0), |r| r.read(buf))?;
        if n == 0 {
            self.job.tracker.finish_entry();
        }
        Ok(n)
    }
}

fn push_solid<W: Write + io::Seek>(
    writer: &mut ArchiveWriter<W>,
    items: Vec<&InputItem>,
    job: &CreateJob,
) -> std::result::Result<(), SzError> {
    let entries = items.iter().map(|i| archive_entry(i)).collect();
    let readers = items.iter().map(|item| SourceReader::new(LazyInput { job, item, reader: None })).collect();
    writer.push_archive_entries(entries, readers)?;
    Ok(())
}
