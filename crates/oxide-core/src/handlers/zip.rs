//! ZIP support via the `zip` crate.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::PathBuf;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use ::zip::read::ZipFile;
use ::zip::result::ZipError;
use ::zip::write::{PreparedZipFile, SimpleFileOptions, ZipFileBuilder};
use ::zip::{AesMode, CompressionMethod as ZipMethod, ZipArchive, ZipWriter};

use super::{dos_to_unix, system_to_dos};
use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::{CreateJob, InputItem, InputKind};
use crate::encryption::{EncryptionMethod, Password};
use crate::error::{IoContext, OxideError, Result};
use crate::formats::CompressionMethod;
use crate::progress::{CancelToken, Forwarded, WorkerProgress};

pub(crate) struct ZipHandler;

/// Symlink targets longer than this are treated as corrupt.
const MAX_LINK_TARGET: u64 = 4096;

fn map_err(err: ZipError) -> OxideError {
    match err {
        ZipError::Io(e) => OxideError::from_decode(e),
        ZipError::InvalidPassword => OxideError::WrongPassword,
        ZipError::UnsupportedArchive(msg) if msg == ZipError::PASSWORD_REQUIRED => OxideError::PasswordRequired,
        ZipError::UnsupportedArchive(msg) => OxideError::Corrupt(format!("unsupported ZIP feature: {msg}")),
        ZipError::InvalidArchive(msg) => OxideError::Corrupt(msg.into_owned()),
        other => OxideError::Corrupt(other.to_string()),
    }
}

fn open(ctx: &ReadContext) -> Result<ZipArchive<BufReader<File>>> {
    let file = File::open(ctx.path).at(ctx.path)?;
    ZipArchive::new(BufReader::with_capacity(256 * 1024, file)).map_err(map_err)
}

fn entry_of<R: Read + ?Sized>(f: &ZipFile<'_, R>) -> Entry {
    let path = match f.name() {
        Ok(name) => name.into_owned(),
        Err(_) => String::from_utf8_lossy(f.name_raw()).into_owned(),
    };
    let kind = if f.is_dir() {
        EntryKind::Directory
    } else if f.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    let mut e = Entry::new(path, kind);
    e.size = Some(f.size());
    e.compressed_size = Some(f.compressed_size());
    e.modified = f.last_modified().and_then(dos_to_unix);
    e.mode = f.unix_mode().map(|m| m & 0o7777);
    e.encrypted = f.encrypted();
    e
}

/// Reads every entry's metadata from the central directory.
fn index(zip: &mut ZipArchive<BufReader<File>>) -> Result<Vec<Entry>> {
    (0..zip.len()).map(|i| zip.by_index_raw(i).map(|f| entry_of(&f)).map_err(map_err)).collect()
}

fn open_entry<'z>(
    zip: &'z mut ZipArchive<BufReader<File>>,
    index: usize,
    entry: &Entry,
    password: Option<&Password>,
) -> Result<ZipFile<'z, BufReader<File>>> {
    let file = match (entry.encrypted, password) {
        (true, Some(pw)) => zip.by_index_decrypt(index, pw.expose().as_bytes()),
        (true, None) => return Err(OxideError::PasswordRequired),
        (false, _) => zip.by_index(index),
    };
    file.map_err(map_err)
}

impl FormatHandler for ZipHandler {
    fn list(&self, ctx: &ReadContext) -> Result<Vec<crate::archive::Entry>> {
        index(&mut open(ctx)?)
    }

    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()> {
        let mut zip = open(ctx)?;
        let index = index(&mut zip)?;
        visitor.begin(Some(&index));
        for (i, entry) in index.iter().enumerate() {
            ctx.tracker.check_cancel()?;
            if !visitor.wants(entry)? {
                continue;
            }
            match entry.kind {
                EntryKind::Directory => visitor.visit(entry, None)?,
                EntryKind::Symlink => {
                    let mut file = open_entry(&mut zip, i, entry, ctx.password)?;
                    let mut target = String::new();
                    file.by_ref().take(MAX_LINK_TARGET).read_to_string(&mut target).map_err(OxideError::from_decode)?;
                    drop(file);
                    let mut link = entry.clone();
                    link.link_target = Some(target);
                    visitor.visit(&link, None)?;
                }
                _ => {
                    let mut file = open_entry(&mut zip, i, entry, ctx.password)?;
                    visitor.visit(entry, Some(&mut file))?;
                }
            }
        }
        Ok(())
    }

    fn create(&self, out: &mut File, job: &CreateJob) -> Result<()> {
        let mut zip = ZipWriter::new(BufWriter::with_capacity(1 << 20, out));
        add_inputs(&mut zip, job)?;
        zip.finish().map_err(map_err)?.flush()?;
        Ok(())
    }

    fn append(&self, ctx: &ReadContext, out: &mut File, job: &CreateJob) -> Result<()> {
        let mut source = open(ctx)?;
        let replaced: HashSet<&str> = job.inputs.iter().map(|i| i.name.as_str()).collect();
        let mut zip = ZipWriter::new(BufWriter::with_capacity(1 << 20, out));
        for i in 0..source.len() {
            ctx.tracker.check_cancel()?;
            let file = source.by_index_raw(i).map_err(map_err)?;
            let name = String::from_utf8_lossy(file.name_raw()).into_owned();
            if replaced.contains(name.trim_end_matches('/')) {
                continue;
            }
            // Copies the compressed (and possibly encrypted) bytes verbatim.
            zip.raw_copy_file(file).map_err(map_err)?;
        }
        add_inputs(&mut zip, job)?;
        zip.finish().map_err(map_err)?.flush()?;
        Ok(())
    }
}

fn add_inputs<W: Write + Seek>(zip: &mut ZipWriter<W>, job: &CreateJob) -> Result<()> {
    let method = match job.method {
        CompressionMethod::Store => ZipMethod::Stored,
        CompressionMethod::Deflate => ZipMethod::Deflated,
        CompressionMethod::Bzip2 => ZipMethod::Bzip2,
        CompressionMethod::Zstd => ZipMethod::Zstd,
        CompressionMethod::Xz => ZipMethod::Xz,
        CompressionMethod::Lzma2 => {
            return Err(OxideError::InvalidOption("ZIP cannot use LZMA2; choose XZ instead".into()));
        }
    };
    // The zip crate's DEFLATE accepts 1-9; level 0 means "store", as in 7-Zip.
    let (method, level) = match (method, job.level) {
        (ZipMethod::Deflated, Some(0)) => (ZipMethod::Stored, None),
        (m, l) => (m, l.map(i64::from)),
    };
    // Encrypted entries can only be written through the ZipWriter itself.
    let parallel_files = job.inputs.iter().filter(|i| is_pooled(i)).count();
    if job.password.is_none() && job.threads > 1 && parallel_files > 1 {
        return add_inputs_parallel(zip, job, method, level);
    }
    for item in &job.inputs {
        job.tracker.check_cancel()?;
        add_one(zip, job, item, method, level)?;
    }
    Ok(())
}

/// Files compressed by worker threads in parallel ZIP creation.
const PARALLEL_FILE_MAX: u64 = 8 * 1024 * 1024;

fn is_pooled(item: &InputItem) -> bool {
    matches!(item.kind, InputKind::File { size } if size > 0 && size <= PARALLEL_FILE_MAX)
}

fn base_options(item: &InputItem) -> SimpleFileOptions {
    let mut base = SimpleFileOptions::default();
    if let Some(dt) = item.modified.and_then(system_to_dos) {
        base = base.last_modified_time(dt);
    }
    if let Some(mode) = item.mode {
        base = base.unix_permissions(mode);
    }
    base
}

fn file_options(item: &InputItem, method: ZipMethod, level: Option<i64>) -> SimpleFileOptions {
    base_options(item)
        .compression_method(method)
        .compression_level(level)
        .large_file(item.size() >= u64::from(u32::MAX))
}

/// Writes one entry directly through the archive writer.
fn add_one<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    job: &CreateJob,
    item: &InputItem,
    method: ZipMethod,
    level: Option<i64>,
) -> Result<()> {
    job.tracker.start_entry(&item.name);
    match &item.kind {
        InputKind::Directory => {
            zip.add_directory(format!("{}/", item.name), base_options(item)).map_err(map_err)?;
        }
        InputKind::Symlink { target } => {
            zip.add_symlink(item.name.as_str(), target.as_str(), base_options(item)).map_err(map_err)?;
        }
        InputKind::File { .. } => {
            let mut opts = file_options(item, method, level);
            if let Some(pw) = job.password {
                let aes = match job.encryption {
                    EncryptionMethod::Aes128 => AesMode::Aes128,
                    EncryptionMethod::Aes256 => AesMode::Aes256,
                };
                opts = opts.with_aes_encryption(aes, pw.expose());
            }
            zip.start_file(item.name.as_str(), opts).map_err(map_err)?;
            job.copy_into(item, zip)?;
        }
    }
    job.tracker.finish_entry();
    Ok(())
}

struct CompressJob {
    index: usize,
    name: String,
    path: PathBuf,
    size: u64,
    options: SimpleFileOptions,
}

/// Compresses one file into memory on a worker thread.
fn compress_one(job: CompressJob, progress: &WorkerProgress, cancel: &CancelToken) -> Result<PreparedZipFile> {
    progress.set_current(&job.name);
    let mut builder = ZipFileBuilder::new(&job.name, job.options).map_err(map_err)?;
    let mut file = File::open(&job.path).at(&job.path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut remaining = job.size;
    while remaining > 0 {
        cancel.check()?;
        let max = buf.len().min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let n = file.read(&mut buf[..max]).at(&job.path)?;
        if n == 0 {
            return Err(OxideError::io(
                &job.path,
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file shrank while it was being archived"),
            ));
        }
        builder.write_all(&buf[..n]).at(&job.path)?;
        remaining -= n as u64;
        progress.add_bytes(n as u64);
    }
    let prepared = builder.finish().map_err(map_err)?;
    progress.file_done();
    Ok(prepared)
}

/// Compresses small files on worker threads while the main thread writes
/// entries in their original order. Large files, folders and links are
/// written inline when their turn comes.
fn add_inputs_parallel<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    job: &CreateJob,
    method: ZipMethod,
    level: Option<i64>,
) -> Result<()> {
    let workers = job.threads as usize;
    let progress = WorkerProgress::default();
    let cancel = job.tracker.cancel().clone();
    let (job_tx, job_rx) = mpsc::sync_channel::<CompressJob>(workers * 2);
    let job_rx = Mutex::new(job_rx);
    let (result_tx, results) = mpsc::channel::<(usize, Result<PreparedZipFile>)>();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (job_rx, result_tx, progress, cancel) = (&job_rx, result_tx.clone(), &progress, &cancel);
            scope.spawn(move || {
                loop {
                    let next = job_rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok(job) = next else { return };
                    let index = job.index;
                    if result_tx.send((index, compress_one(job, progress, cancel))).is_err() {
                        return;
                    }
                }
            });
        }
        drop(result_tx);
        let mut job_tx = Some(job_tx);
        let result = write_in_order(zip, job, method, level, &mut job_tx, &results, &progress, workers);
        // Closing the queue lets workers exit after at most one more (small) file.
        drop(job_tx);
        result
    })
}

#[allow(clippy::too_many_arguments)]
fn write_in_order<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    job: &CreateJob,
    method: ZipMethod,
    level: Option<i64>,
    job_tx: &mut Option<mpsc::SyncSender<CompressJob>>,
    results: &mpsc::Receiver<(usize, Result<PreparedZipFile>)>,
    progress: &WorkerProgress,
    workers: usize,
) -> Result<()> {
    let inputs = &job.inputs;
    let mut ready: HashMap<usize, Result<PreparedZipFile>> = HashMap::new();
    let mut seen = Forwarded::default();
    let mut next = 0; // next entry to write
    let mut outstanding = 0usize;

    // Writes every entry up to `limit` whose turn has come and whose data is ready.
    let emit = |zip: &mut ZipWriter<W>,
                ready: &mut HashMap<usize, Result<PreparedZipFile>>,
                next: &mut usize,
                limit: usize| {
        while *next < limit {
            let item = &inputs[*next];
            if is_pooled(item) {
                let Some(prepared) = ready.remove(next) else { break };
                job.tracker.start_entry(&item.name);
                zip.add_prepared_file(prepared?).map_err(map_err)?;
            } else {
                add_one(zip, job, item, method, level)?;
            }
            *next += 1;
        }
        Ok::<_, OxideError>(())
    };

    for (index, item) in inputs.iter().enumerate() {
        job.tracker.check_cancel()?;
        if is_pooled(item) {
            let task = CompressJob {
                index,
                name: item.name.clone(),
                path: item.source.clone(),
                size: item.size(),
                options: file_options(item, method, level),
            };
            let sent = job_tx.as_ref().is_some_and(|tx| tx.send(task).is_ok());
            if !sent {
                return Err(OxideError::from(std::io::Error::other("ZIP compression threads stopped")));
            }
            outstanding += 1;
        }
        while let Ok((i, r)) = results.try_recv() {
            ready.insert(i, r);
            outstanding -= 1;
        }
        // Keep a bounded number of compressed files waiting in memory.
        while outstanding > workers * 4 {
            receive(results, &mut ready, &mut outstanding, job, progress, &mut seen)?;
            emit(zip, &mut ready, &mut next, index + 1)?;
        }
        progress.forward(job.tracker, &mut seen);
        emit(zip, &mut ready, &mut next, index + 1)?;
    }
    *job_tx = None;
    while next < inputs.len() {
        emit(zip, &mut ready, &mut next, inputs.len())?;
        if next < inputs.len() {
            receive(results, &mut ready, &mut outstanding, job, progress, &mut seen)?;
        }
    }
    progress.forward(job.tracker, &mut seen);
    Ok(())
}

/// Waits (briefly) for one compressed file, forwarding progress meanwhile.
fn receive(
    results: &mpsc::Receiver<(usize, Result<PreparedZipFile>)>,
    ready: &mut HashMap<usize, Result<PreparedZipFile>>,
    outstanding: &mut usize,
    job: &CreateJob,
    progress: &WorkerProgress,
    seen: &mut Forwarded,
) -> Result<()> {
    loop {
        progress.forward(job.tracker, seen);
        job.tracker.check_cancel()?;
        match results.recv_timeout(Duration::from_millis(50)) {
            Ok((i, r)) => {
                ready.insert(i, r);
                *outstanding -= 1;
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(OxideError::from(std::io::Error::other("ZIP compression threads stopped")));
            }
        }
    }
}
