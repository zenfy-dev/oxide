//! ZIP support via the `zip` crate.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};

use ::zip::read::ZipFile;
use ::zip::result::ZipError;
use ::zip::write::SimpleFileOptions;
use ::zip::{AesMode, CompressionMethod as ZipMethod, ZipArchive, ZipWriter};

use super::{dos_to_unix, system_to_dos};
use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::{CreateJob, InputKind};
use crate::encryption::{EncryptionMethod, Password};
use crate::error::{IoContext, OxideError, Result};
use crate::formats::CompressionMethod;

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
        let mut zip = open(ctx)?;
        (0..zip.len()).map(|i| zip.by_index_raw(i).map(|f| entry_of(&f)).map_err(map_err)).collect()
    }

    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()> {
        let mut zip = open(ctx)?;
        let index = self.list(ctx)?;
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
    let aes = match job.encryption {
        EncryptionMethod::Aes128 => AesMode::Aes128,
        EncryptionMethod::Aes256 => AesMode::Aes256,
    };
    for item in &job.inputs {
        job.tracker.check_cancel()?;
        job.tracker.start_entry(&item.name);
        let mut base = SimpleFileOptions::default();
        if let Some(dt) = item.modified.and_then(system_to_dos) {
            base = base.last_modified_time(dt);
        }
        if let Some(mode) = item.mode {
            base = base.unix_permissions(mode);
        }
        match &item.kind {
            InputKind::Directory => {
                zip.add_directory(format!("{}/", item.name), base).map_err(map_err)?;
            }
            InputKind::Symlink { target } => {
                zip.add_symlink(item.name.as_str(), target.as_str(), base).map_err(map_err)?;
            }
            InputKind::File { size } => {
                // The zip crate's DEFLATE accepts 1-9; level 0 means "store", as in 7-Zip.
                let (method, level) = match (method, job.level) {
                    (ZipMethod::Deflated, Some(0)) => (ZipMethod::Stored, None),
                    (m, l) => (m, l.map(i64::from)),
                };
                let mut opts =
                    base.compression_method(method).compression_level(level).large_file(*size >= u64::from(u32::MAX));
                if let Some(pw) = job.password {
                    opts = opts.with_aes_encryption(aes, pw.expose());
                }
                zip.start_file(item.name.as_str(), opts).map_err(map_err)?;
                job.copy_into(item, zip)?;
            }
        }
        job.tracker.finish_entry();
    }
    Ok(())
}
