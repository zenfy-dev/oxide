//! Archive creation: options, input collection, stream codecs, and the
//! shared job state handed to format handlers.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::archive::{OperationSummary, parent_dir};
use crate::encryption::{EncryptionMethod, Password};
use crate::error::{IoContext, OxideError, Result};
use crate::formats::{ArchiveFormat, CompressionMethod};
use crate::handlers;
use crate::progress::{Operation, Progress, Tracker};

/// Settings for [`create`] and [`add`](crate::add).
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Output format. `None` infers it from the output file's extension.
    pub format: Option<ArchiveFormat>,
    /// Compression method. `None` uses the format's default.
    pub method: Option<CompressionMethod>,
    /// Compression level. `None` uses the method's default.
    pub level: Option<i32>,
    /// Encrypt the archive with this password (ZIP and 7z only).
    pub password: Option<Password>,
    /// Encryption scheme. `None` picks the strongest the format supports.
    pub encryption: Option<EncryptionMethod>,
    /// Keep each input's relative directory structure. When false, every
    /// file is stored at the archive root under its own name.
    pub preserve_paths: bool,
    /// Include dot-files and (on Windows) files with the hidden attribute.
    pub include_hidden: bool,
    /// Archive the targets of symbolic links instead of the links themselves.
    pub follow_symlinks: bool,
    /// Worker threads for formats that support parallel compression; 0 = all cores.
    pub threads: u32,
    /// Replace the output file if it exists.
    pub overwrite: bool,
}

impl Default for CreateOptions {
    fn default() -> Self {
        CreateOptions {
            format: None,
            method: None,
            level: None,
            password: None,
            encryption: None,
            preserve_paths: true,
            include_hidden: true,
            follow_symlinks: false,
            threads: 0,
            overwrite: false,
        }
    }
}

/// What an input path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputKind {
    File { size: u64 },
    Directory,
    Symlink { target: String },
}

/// One filesystem object to be archived.
#[derive(Debug, Clone)]
pub(crate) struct InputItem {
    pub source: PathBuf,
    /// `/`-separated path inside the archive, without a trailing slash.
    pub name: String,
    pub kind: InputKind,
    pub modified: Option<SystemTime>,
    pub mode: Option<u32>,
}

impl InputItem {
    pub(crate) fn size(&self) -> u64 {
        match self.kind {
            InputKind::File { size } => size,
            _ => 0,
        }
    }
}

/// Validated settings and inputs for one create/add operation.
pub(crate) struct CreateJob<'a> {
    pub format: ArchiveFormat,
    pub inputs: Vec<InputItem>,
    pub method: CompressionMethod,
    pub level: Option<i32>,
    pub password: Option<&'a Password>,
    pub encryption: EncryptionMethod,
    pub threads: u32,
    pub tracker: &'a Tracker<'a>,
    warnings: Vec<String>,
    /// Copy buffer reused for every input file.
    buf: std::cell::RefCell<Vec<u8>>,
}

impl<'a> CreateJob<'a> {
    pub(crate) fn prepare(
        format: ArchiveFormat,
        sources: &[PathBuf],
        exclude: Option<&Path>,
        options: &'a CreateOptions,
        tracker: &'a Tracker<'a>,
    ) -> Result<Self> {
        let info = format.info();
        if !info.can_create {
            return Err(match format {
                ArchiveFormat::SevenZ => OxideError::FeatureDisabled { format: "7z", feature: "sevenz" },
                _ => OxideError::Unsupported { format: format.name(), operation: "creation" },
            });
        }
        if sources.is_empty() {
            return Err(OxideError::InvalidOption("no input files given".into()));
        }
        let method = options.method.unwrap_or_else(|| format.default_method());
        if !info.methods.contains(&method) {
            return Err(OxideError::InvalidOption(format!(
                "{format} cannot use the {method} method (available: {})",
                info.methods.iter().map(|m| m.name()).collect::<Vec<_>>().join(", ")
            )));
        }
        let level = resolve_level(method, options.level)?;
        let password = options.password.as_ref().filter(|p| !p.is_empty());
        let encryption = match (password, options.encryption) {
            (None, _) => EncryptionMethod::Aes256,
            (Some(_), _) if info.encryption.is_empty() => {
                return Err(OxideError::Unsupported { format: format.name(), operation: "encryption" });
            }
            (Some(_), Some(m)) if !info.encryption.contains(&m) => {
                return Err(OxideError::InvalidOption(format!("{format} does not support {m} encryption")));
            }
            (Some(_), m) => m.unwrap_or(info.encryption[0]),
        };

        let (inputs, warnings) = collect_inputs(sources, options, exclude)?;
        if !info.multi_file {
            let files = inputs.iter().filter(|i| matches!(i.kind, InputKind::File { .. })).count();
            if files != 1 || inputs.len() != 1 {
                return Err(OxideError::InvalidOption(format!(
                    "{format} compresses exactly one file; use a TAR-based format for folders or multiple files"
                )));
            }
        }
        let threads = match options.threads {
            0 => std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1),
            n => n,
        };
        let bytes = inputs.iter().map(InputItem::size).sum();
        tracker.set_totals(Some(inputs.len() as u64), Some(bytes));
        let buf = std::cell::RefCell::new(vec![0u8; 256 * 1024]);
        Ok(CreateJob { format, inputs, method, level, password, encryption, threads, tracker, warnings, buf })
    }

    /// Opens an input file for reading with progress, cancellation and a
    /// guard against the file changing size mid-read.
    pub(crate) fn open(&self, item: &InputItem) -> Result<TrackedReader<'a, File>> {
        let file = File::open(&item.source).at(&item.source)?;
        Ok(TrackedReader { inner: file, tracker: self.tracker, remaining: item.size(), path: item.source.clone() })
    }

    /// Copies an input file's contents into `out`.
    pub(crate) fn copy_into(&self, item: &InputItem, out: &mut dyn Write) -> Result<()> {
        let mut reader = self.open(item)?;
        let mut buf = self.buf.borrow_mut();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(self.map_err(e, &item.source)),
            };
            out.write_all(&buf[..n]).map_err(|e| self.map_err(e, &item.source))?;
        }
    }

    /// Converts an error raised mid-write, preferring `Cancelled` when the
    /// token fired (the error text from a library is then irrelevant).
    pub(crate) fn map_err(&self, err: io::Error, path: &Path) -> OxideError {
        if self.tracker.cancel().is_cancelled() { OxideError::Cancelled } else { OxideError::io(path, err) }
    }

    pub(crate) fn summary(&self, tracker: &Tracker) -> OperationSummary {
        let mut s = OperationSummary {
            bytes: tracker.bytes_done(),
            warnings: self.warnings.clone(),
            elapsed: tracker.elapsed(),
            ..OperationSummary::default()
        };
        for item in &self.inputs {
            match item.kind {
                InputKind::File { .. } => s.files += 1,
                InputKind::Directory => s.directories += 1,
                InputKind::Symlink { .. } => s.links += 1,
            }
        }
        s
    }
}

/// A file reader that reports progress, honours cancellation, and refuses
/// to read past (or stop short of) the size recorded when inputs were collected.
pub(crate) struct TrackedReader<'a, R> {
    inner: R,
    tracker: &'a Tracker<'a>,
    remaining: u64,
    path: PathBuf,
}

impl<R: Read> Read for TrackedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.tracker.cancel().is_cancelled() {
            return Err(io::Error::other("operation cancelled"));
        }
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let max = buf.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..max])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} shrank while it was being archived", self.path.display()),
            ));
        }
        self.remaining -= n as u64;
        self.tracker.add_bytes(n as u64);
        Ok(n)
    }
}

fn resolve_level(method: CompressionMethod, level: Option<i32>) -> Result<Option<i32>> {
    match (method.level_range(), level) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(OxideError::InvalidOption(format!("the {method} method has no compression levels"))),
        (Some(r), None) => Ok(Some(r.default)),
        (Some(r), Some(l)) if (r.min..=r.max).contains(&l) => Ok(Some(l)),
        (Some(r), Some(l)) => Err(OxideError::InvalidOption(format!(
            "compression level {l} is out of range for {method} ({}-{})",
            r.min, r.max
        ))),
    }
}

/// Is this directory entry hidden on the current platform?
fn is_hidden(path: &Path, meta: Option<&fs::Metadata>) -> bool {
    let dot = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') && n != "." && n != "..");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if meta.is_some_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0) {
            return true;
        }
    }
    #[cfg(not(windows))]
    let _ = meta;
    dot
}

fn unix_mode(meta: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn path_to_archive_name(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Walks `sources` and produces the list of items to archive, in a
/// deterministic (sorted) order. `exclude` is skipped if encountered, so an
/// archive written inside one of its own input folders never includes itself.
pub(crate) fn collect_inputs(
    sources: &[PathBuf],
    options: &CreateOptions,
    exclude: Option<&Path>,
) -> Result<(Vec<InputItem>, Vec<String>)> {
    // Resolving every walked path is expensive, so only paths whose file name
    // matches the output's are resolved and compared.
    let exclude = exclude.and_then(|p| Some((fs::canonicalize(p).ok()?, p.file_name()?.to_os_string())));
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();

    for source in sources {
        let meta = fs::symlink_metadata(source).at(source)?;
        // Name of the top-level item; `None` for "." and similar, whose
        // contents then go at the archive root.
        let base = source.file_name().map(|n| n.to_string_lossy().into_owned());
        let is_dir = meta.is_dir() || (options.follow_symlinks && meta.file_type().is_symlink() && source.is_dir());

        let walker =
            WalkDir::new(source).follow_links(options.follow_symlinks).sort_by_file_name().into_iter().filter_entry(
                |e| e.depth() == 0 || options.include_hidden || !is_hidden(e.path(), e.metadata().ok().as_ref()),
            );

        for entry in walker {
            let entry = entry.map_err(|e| {
                let path = e.path().map(Path::to_path_buf).unwrap_or_else(|| source.clone());
                match e.into_io_error() {
                    Some(io) => OxideError::io(path, io),
                    None => OxideError::InvalidOption(format!("symlink loop at {}", path.display())),
                }
            })?;
            if entry.depth() == 0 && is_dir && base.is_none() {
                continue;
            }
            let path = entry.path();
            if let Some((ex, ex_name)) = &exclude
                && entry.file_name() == ex_name.as_os_str()
                && fs::canonicalize(path).ok().as_ref() == Some(ex)
            {
                continue;
            }
            let rel = path.strip_prefix(source).unwrap_or(path);
            let name = if options.preserve_paths {
                let rel = path_to_archive_name(rel);
                match (&base, rel.is_empty()) {
                    (Some(b), true) => b.clone(),
                    (Some(b), false) => format!("{b}/{rel}"),
                    (None, _) => rel,
                }
            } else {
                entry.file_name().to_string_lossy().into_owned()
            };
            if name.is_empty() {
                continue;
            }
            let ft = entry.file_type();
            let meta =
                if ft.is_symlink() { fs::symlink_metadata(path) } else { entry.metadata().map_err(io::Error::from) }
                    .at(path)?;
            let kind = if ft.is_dir() {
                if !options.preserve_paths {
                    continue;
                }
                InputKind::Directory
            } else if ft.is_file() {
                InputKind::File { size: meta.len() }
            } else if ft.is_symlink() {
                let target = fs::read_link(path).at(path)?;
                InputKind::Symlink { target: target.to_string_lossy().replace('\\', "/") }
            } else {
                warnings.push(format!("skipped special file {}", path.display()));
                continue;
            };
            if !seen.insert(name.clone()) {
                return Err(OxideError::InvalidOption(format!(
                    "two inputs map to the same archive path {name:?}; enable path preservation or rename one"
                )));
            }
            items.push(InputItem {
                source: path.to_path_buf(),
                name,
                kind,
                modified: meta.modified().ok(),
                mode: unix_mode(&meta),
            });
        }
    }
    Ok((items, warnings))
}

/// Creates a new archive at `output` from files and folders in `sources`.
///
/// Data is written to a temporary file in the output directory and moved
/// into place only once the archive is complete, so failure or cancellation
/// never leaves a truncated archive behind.
pub fn create(
    sources: &[PathBuf],
    output: &Path,
    options: &CreateOptions,
    progress: &Progress,
) -> Result<OperationSummary> {
    let format = options.format.or_else(|| ArchiveFormat::from_path(output)).ok_or_else(|| {
        OxideError::InvalidOption(format!(
            "cannot infer the archive format from {:?}; use a known extension or choose a format",
            output.file_name().unwrap_or_default()
        ))
    })?;
    let handler = handlers::handler_for(format)?;
    if output.is_dir() {
        return Err(OxideError::InvalidOption(format!("{} is a directory", output.display())));
    }
    if !options.overwrite && fs::symlink_metadata(output).is_ok() {
        return Err(OxideError::AlreadyExists(output.to_path_buf()));
    }

    let tracker = Tracker::new(progress, Operation::Create);
    let job = CreateJob::prepare(format, sources, Some(output), options, &tracker)?;
    tracing::debug!(%format, inputs = job.inputs.len(), "creating archive");

    let parent = parent_dir(output);
    fs::create_dir_all(&parent).at(&parent)?;
    let mut tmp = tempfile::Builder::new().prefix(".oxide-").suffix(".tmp").tempfile_in(&parent).at(&parent)?;
    let result = handler.create(tmp.as_file_mut(), &job);
    if progress.cancel_token().is_cancelled() {
        return Err(OxideError::Cancelled);
    }
    result?;
    tmp.as_file().sync_all().at(tmp.path())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o644));
    }
    let persisted = if options.overwrite { tmp.persist(output) } else { tmp.persist_noclobber(output) };
    persisted.map_err(|e| match e.error.kind() {
        io::ErrorKind::AlreadyExists => OxideError::AlreadyExists(output.to_path_buf()),
        _ => OxideError::io(output, e.error),
    })?;
    tracker.finish();
    Ok(job.summary(&tracker))
}

/// Streaming compression codecs used by the TAR family and single-file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

/// A compressing writer that must be explicitly finalised.
pub(crate) trait FinishWrite: Write {
    fn finish(self: Box<Self>) -> io::Result<()>;
}

impl<W: Write> FinishWrite for flate2::write::GzEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish()?.flush()
    }
}

impl<W: Write> FinishWrite for bzip2::write::BzEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish()?.flush()
    }
}

impl<W: Write> FinishWrite for liblzma::write::XzEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish()?.flush()
    }
}

impl<W: Write> FinishWrite for zstd::stream::write::Encoder<'_, W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish()?.flush()
    }
}

/// Input chunk size for parallel gzip/bzip2. Each chunk becomes its own
/// gzip member / bzip2 stream; concatenations of these are valid files
/// that every decompressor reads in full (as `pigz` and `pbzip2` produce).
const PARALLEL_CHUNK: usize = 1024 * 1024;

type ChunkFn = dyn Fn(&[u8]) -> io::Result<Vec<u8>> + Send + Sync;

/// Compresses fixed-size chunks on several threads and writes them in order.
pub(crate) struct ParallelEncoder<W: Write> {
    out: W,
    chunk: Vec<u8>,
    jobs: Option<std::sync::mpsc::SyncSender<(u64, Vec<u8>)>>,
    results: std::sync::mpsc::Receiver<(u64, io::Result<Vec<u8>>)>,
    workers: Vec<std::thread::JoinHandle<()>>,
    submitted: u64,
    written: u64,
    ready: std::collections::BTreeMap<u64, io::Result<Vec<u8>>>,
}

impl<W: Write> ParallelEncoder<W> {
    fn new(out: W, threads: u32, compress: impl Fn(&[u8]) -> io::Result<Vec<u8>> + Send + Sync + 'static) -> Self {
        let threads = threads.max(1) as usize;
        let compress: std::sync::Arc<ChunkFn> = std::sync::Arc::new(compress);
        // The queue bound keeps at most a few chunks per thread in memory.
        let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<(u64, Vec<u8>)>(threads * 2);
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        let (result_tx, results) = std::sync::mpsc::channel();
        let workers = (0..threads)
            .filter_map(|i| {
                let (jobs, results, compress) = (job_rx.clone(), result_tx.clone(), compress.clone());
                std::thread::Builder::new()
                    .name(format!("oxide-compress-{i}"))
                    .spawn(move || {
                        loop {
                            let job = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv();
                            let Ok((seq, data)) = job else { return };
                            if results.send((seq, compress(&data))).is_err() {
                                return;
                            }
                        }
                    })
                    .ok()
            })
            .collect();
        ParallelEncoder {
            out,
            chunk: Vec::with_capacity(PARALLEL_CHUNK),
            jobs: Some(job_tx),
            results,
            workers,
            submitted: 0,
            written: 0,
            ready: std::collections::BTreeMap::new(),
        }
    }

    fn submit_chunk(&mut self) -> io::Result<()> {
        let data = std::mem::replace(&mut self.chunk, Vec::with_capacity(PARALLEL_CHUNK));
        let seq = self.submitted;
        self.submitted += 1;
        if self.workers.is_empty() {
            return Err(io::Error::other("could not start compression threads"));
        }
        let sent = self.jobs.as_ref().is_some_and(|tx| tx.send((seq, data)).is_ok());
        if !sent {
            return Err(io::Error::other("compression threads stopped"));
        }
        self.write_ready(false)
    }

    /// Writes finished chunks in order; with `wait`, until all are written.
    fn write_ready(&mut self, wait: bool) -> io::Result<()> {
        loop {
            while let Some(result) = self.ready.remove(&self.written) {
                self.out.write_all(&result?)?;
                self.written += 1;
            }
            if self.written == self.submitted {
                return Ok(());
            }
            let next = if wait { self.results.recv().ok() } else { self.results.try_recv().ok() };
            match next {
                Some((seq, result)) => {
                    self.ready.insert(seq, result);
                }
                None if wait => return Err(io::Error::other("compression threads stopped")),
                None => return Ok(()),
            }
        }
    }
}

impl<W: Write> Write for ParallelEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(PARALLEL_CHUNK - self.chunk.len());
        self.chunk.extend_from_slice(&buf[..n]);
        if self.chunk.len() == PARALLEL_CHUNK {
            self.submit_chunk()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Chunks are only complete once full; `finish` writes the remainder.
        self.out.flush()
    }
}

impl<W: Write> FinishWrite for ParallelEncoder<W> {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        // Always emit at least one member so empty input is still a valid file.
        if !self.chunk.is_empty() || self.submitted == 0 {
            self.submit_chunk()?;
        }
        self.write_ready(true)?;
        self.jobs = None;
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.out.flush()
    }
}

impl<W: Write> Drop for ParallelEncoder<W> {
    fn drop(&mut self) {
        self.jobs = None;
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Pass-through for uncompressed TAR.
pub(crate) struct Plain<W: Write>(pub W);

impl<W: Write> Write for Plain<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write> FinishWrite for Plain<W> {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.0.flush()
    }
}

impl Codec {
    /// Wraps a buffered reader in a decompressor. Concatenated streams
    /// (multi-member gzip, multi-stream bzip2/xz, multi-frame zstd) are read fully.
    pub(crate) fn decoder<'r, R: BufRead + 'r>(self, reader: R) -> Result<Box<dyn Read + 'r>> {
        Ok(match self {
            Codec::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(reader)),
            Codec::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(reader)),
            Codec::Xz => Box::new(liblzma::bufread::XzDecoder::new_multi_decoder(reader)),
            Codec::Zstd => Box::new(zstd::stream::read::Decoder::with_buffer(reader)?),
        })
    }

    /// Wraps a writer in a compressor. `threads > 1` enables parallel
    /// compression for XZ and Zstandard.
    pub(crate) fn encoder<'w, W: Write + 'w>(
        self,
        writer: W,
        level: i32,
        threads: u32,
    ) -> Result<Box<dyn FinishWrite + 'w>> {
        let level_u = level.max(0) as u32;
        Ok(match self {
            Codec::Gzip if threads > 1 => Box::new(ParallelEncoder::new(writer, threads, move |chunk: &[u8]| {
                let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level_u));
                enc.write_all(chunk)?;
                enc.finish()
            })),
            Codec::Bzip2 if threads > 1 => Box::new(ParallelEncoder::new(writer, threads, move |chunk: &[u8]| {
                let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level_u.clamp(1, 9)));
                enc.write_all(chunk)?;
                enc.finish()
            })),
            Codec::Gzip => Box::new(flate2::write::GzEncoder::new(writer, flate2::Compression::new(level_u))),
            Codec::Bzip2 => {
                Box::new(bzip2::write::BzEncoder::new(writer, bzip2::Compression::new(level_u.clamp(1, 9))))
            }
            Codec::Xz if threads > 1 => {
                let stream = liblzma::stream::MtStreamBuilder::new()
                    .threads(threads)
                    .preset(level_u)
                    .check(liblzma::stream::Check::Crc64)
                    .encoder()
                    .map_err(|e| OxideError::Io { path: None, source: e.into() })?;
                Box::new(liblzma::write::XzEncoder::new_stream(writer, stream))
            }
            Codec::Xz => Box::new(liblzma::write::XzEncoder::new(writer, level_u)),
            Codec::Zstd => {
                let mut enc = zstd::stream::write::Encoder::new(writer, level)?;
                enc.include_checksum(true)?;
                if threads > 1 {
                    enc.multithread(threads)?;
                }
                Box::new(enc)
            }
        })
    }
}
