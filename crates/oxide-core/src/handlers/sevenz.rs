//! 7z support via the pure-Rust `sevenz-rust2` crate.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime};

use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
use sevenz_rust2::{
    ArchiveEntry, ArchiveReader, ArchiveWriter, EncoderConfiguration, EncoderMethod, Error as SzError,
    Password as SzPassword, PreparedBlock, SourceReader, prepare_block,
};

use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::{CreateJob, InputItem, InputKind};
use crate::error::{IoContext, OxideError, Result};
use crate::extraction::system_time_to_unix;
use crate::formats::CompressionMethod;
use crate::progress::{CancelToken, Forwarded, WorkerProgress};

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
/// Solid block size range when compressing blocks on several threads.
const PARALLEL_BLOCK_MIN: u64 = 8 * 1024 * 1024;
const PARALLEL_BLOCK_MAX: u64 = 64 * 1024 * 1024;
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
        let sz_err = |e: SzError| if job.tracker.cancel().is_cancelled() { OxideError::Cancelled } else { map_err(e) };

        // Plan solid blocks. With several threads, blocks are sized so there is
        // enough of them to keep every thread busy.
        let workers = job.threads.max(1) as usize;
        let file_bytes: u64 = job.inputs.iter().map(InputItem::size).sum();
        let block_target = if workers > 1 {
            (file_bytes / (workers as u64 * 2)).clamp(PARALLEL_BLOCK_MIN, PARALLEL_BLOCK_MAX)
        } else {
            SOLID_BLOCK_BYTES
        };
        let mut blocks: Vec<Vec<BlockFile>> = Vec::new();
        let mut block_bytes = 0u64;
        for item in &job.inputs {
            if let InputKind::File { size } = item.kind
                && size > 0
            {
                let full = blocks.last().is_none_or(|b| {
                    !b.is_empty() && (block_bytes + size > block_target || b.len() >= SOLID_BLOCK_FILES)
                });
                if full {
                    blocks.push(Vec::new());
                    block_bytes = 0;
                }
                block_bytes += size;
                if let Some(b) = blocks.last_mut() {
                    b.push(BlockFile {
                        path: item.source.clone(),
                        name: item.name.clone(),
                        size,
                        entry: archive_entry(item),
                    });
                }
            }
        }
        // Parallel across blocks when there are enough; otherwise let LZMA2
        // split a single large block across threads instead.
        let parallel = workers > 1 && blocks.len() > 1;

        let mut methods: Vec<EncoderConfiguration> = Vec::new();
        if let Some(pw) = job.password {
            methods.push(AesEncoderOptions::new(SzPassword::new(pw.expose())).into());
        }
        let level = job.level.unwrap_or(6).clamp(0, 9) as u32;
        methods.push(match job.method {
            CompressionMethod::Store => EncoderConfiguration::new(EncoderMethod::COPY),
            CompressionMethod::Lzma2 if workers > 1 && !parallel => {
                Lzma2Options::from_level_mt(level, job.threads, 16 * 1024 * 1024).into()
            }
            CompressionMethod::Lzma2 => Lzma2Options::from_level(level).into(),
            other => return Err(OxideError::InvalidOption(format!("7z cannot use the {other} method"))),
        });
        writer.set_content_methods(methods.clone());
        writer.set_encrypt_header(job.password.is_some());

        // Entries without data: folders, empty files, and links (tiny).
        for item in &job.inputs {
            job.tracker.check_cancel()?;
            match &item.kind {
                InputKind::File { size } if *size > 0 => continue,
                InputKind::File { .. } | InputKind::Directory => {
                    job.tracker.start_entry(&item.name);
                    writer.push_archive_entry(archive_entry(item), None::<&[u8]>).map_err(sz_err)?;
                }
                InputKind::Symlink { target } => {
                    job.tracker.start_entry(&item.name);
                    let mut entry = archive_entry(item);
                    entry.windows_attributes = UNIX_EXTENSION | ATTR_ARCHIVE | ((S_IFLNK | 0o777) << 16);
                    writer.push_archive_entry(entry, Some(target.as_bytes())).map_err(sz_err)?;
                }
            }
            job.tracker.finish_entry();
        }

        compress_blocks(&mut writer, blocks, Arc::new(methods), if parallel { workers } else { 1 }, job)?;
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

/// One input file assigned to a solid block.
struct BlockFile {
    path: std::path::PathBuf,
    name: String,
    size: u64,
    entry: ArchiveEntry,
}

/// Reads one input file for a block: opened on first read (a block may hold
/// thousands of files), honours cancellation, and refuses files that change
/// size while being archived.
struct BlockReader<'s> {
    file: BlockFile,
    reader: Option<File>,
    remaining: u64,
    finished: bool,
    shared: &'s WorkerProgress,
    cancel: &'s CancelToken,
}

impl Read for BlockReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("operation cancelled"));
        }
        if self.reader.is_none() {
            self.reader = Some(File::open(&self.file.path)?);
            self.shared.set_current(&self.file.name);
        }
        let max = buf.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = match &mut self.reader {
            Some(r) if max > 0 => r.read(&mut buf[..max])?,
            _ => 0,
        };
        if n == 0 && self.remaining > 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} shrank while it was being archived", self.file.path.display()),
            ));
        }
        self.remaining -= n as u64;
        self.shared.add_bytes(n as u64);
        if self.remaining == 0 && !std::mem::replace(&mut self.finished, true) {
            self.shared.file_done();
            self.reader = None; // close as soon as the file is done
        }
        Ok(n)
    }
}

/// Compresses `blocks` on `workers` threads and appends them in order.
fn compress_blocks<W: Write + io::Seek>(
    writer: &mut ArchiveWriter<W>,
    blocks: Vec<Vec<BlockFile>>,
    methods: Arc<Vec<EncoderConfiguration>>,
    workers: usize,
    job: &CreateJob,
) -> Result<()> {
    let total = blocks.len();
    if total == 0 {
        return Ok(());
    }
    let shared = WorkerProgress::default();
    let cancel = job.tracker.cancel().clone();
    let slots: Vec<Mutex<Option<Vec<BlockFile>>>> = blocks.into_iter().map(|b| Mutex::new(Some(b))).collect();
    let next = AtomicUsize::new(0);
    let appended = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<(usize, std::result::Result<PreparedBlock, SzError>)>();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (tx, methods) = (tx.clone(), methods.clone());
            let (slots, next, appended, abort, shared, cancel) = (&slots, &next, &appended, &abort, &shared, &cancel);
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        return;
                    }
                    // Stay at most two blocks per thread ahead of the writer, bounding memory.
                    while i >= appended.load(Ordering::Relaxed) + workers * 2 {
                        if abort.load(Ordering::Relaxed) || cancel.is_cancelled() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if abort.load(Ordering::Relaxed) || cancel.is_cancelled() {
                        return;
                    }
                    let files = slots[i].lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
                    let (entries, readers): (Vec<_>, Vec<_>) = files
                        .into_iter()
                        .map(|f| {
                            let entry = f.entry.clone();
                            let remaining = f.size;
                            let reader =
                                BlockReader { file: f, reader: None, remaining, finished: false, shared, cancel };
                            (entry, SourceReader::new(reader))
                        })
                        .collect();
                    if tx.send((i, prepare_block(methods.clone(), entries, readers))).is_err() {
                        return;
                    }
                }
            });
        }
        drop(tx);

        let result = append_in_order(writer, &rx, total, &appended, &shared, job);
        if result.is_err() {
            abort.store(true, Ordering::Relaxed);
        }
        result
    })
}

/// Receives prepared blocks, appends them in block order, and forwards progress.
fn append_in_order<W: Write + io::Seek>(
    writer: &mut ArchiveWriter<W>,
    rx: &mpsc::Receiver<(usize, std::result::Result<PreparedBlock, SzError>)>,
    total: usize,
    appended: &AtomicUsize,
    shared: &WorkerProgress,
    job: &CreateJob,
) -> Result<()> {
    let sz_err = |e: SzError| if job.tracker.cancel().is_cancelled() { OxideError::Cancelled } else { map_err(e) };
    let mut ready = std::collections::BTreeMap::new();
    let mut seen = Forwarded::default();
    let mut want = 0;
    while want < total {
        shared.forward(job.tracker, &mut seen);
        job.tracker.check_cancel()?;

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((i, block)) => {
                ready.insert(i, block);
                while let Some(block) = ready.remove(&want) {
                    writer.push_prepared_block(block.map_err(sz_err)?).map_err(sz_err)?;
                    want += 1;
                    appended.store(want, Ordering::Relaxed);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                job.tracker.check_cancel()?;
                return Err(OxideError::from(io::Error::other("7z compression threads stopped early")));
            }
        }
    }
    shared.forward(job.tracker, &mut seen);
    Ok(())
}
