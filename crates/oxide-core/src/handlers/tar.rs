//! TAR, optionally wrapped in gzip, bzip2, XZ or Zstandard.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};

use ::tar::{Archive, Builder, EntryType, Header};

use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::{CreateJob, FinishWrite, InputKind, Plain};
use crate::error::{IoContext, OxideError, Result};
use crate::extraction::system_time_to_unix;
use crate::progress::CountingReader;

pub(crate) struct TarHandler;

fn open_stream<'a>(ctx: &ReadContext<'a>) -> Result<Box<dyn Read + 'a>> {
    let file = File::open(ctx.path).at(ctx.path)?;
    let input = BufReader::with_capacity(256 * 1024, CountingReader::new(file, ctx.tracker));
    match ctx.format.codec() {
        Some(codec) => codec.decoder(input),
        None => Ok(Box::new(input)),
    }
}

/// Tar errors after the file was opened are always data problems.
fn corrupt(err: io::Error) -> OxideError {
    OxideError::Corrupt(err.to_string())
}

fn entry_of<R: Read>(item: &::tar::Entry<'_, R>) -> Option<Entry> {
    let header = item.header();
    let path = String::from_utf8_lossy(&item.path_bytes()).into_owned();
    let kind = match header.entry_type() {
        // Pre-POSIX archives mark directories only with a trailing slash.
        EntryType::Regular | EntryType::Continuous if path.ends_with('/') => EntryKind::Directory,
        EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => EntryKind::File,
        EntryType::Directory => EntryKind::Directory,
        EntryType::Symlink => EntryKind::Symlink,
        EntryType::Link => EntryKind::Hardlink,
        // Metadata records already folded into the following entry by the tar crate.
        EntryType::XGlobalHeader | EntryType::XHeader | EntryType::GNULongName | EntryType::GNULongLink => {
            return None;
        }
        _ => EntryKind::Other,
    };
    let mut e = Entry::new(path, kind);
    e.size = Some(item.size());
    e.modified = header.mtime().ok().and_then(|m| i64::try_from(m).ok());
    e.mode = header.mode().ok().map(|m| m & 0o7777);
    e.link_target = item.link_name_bytes().map(|b| String::from_utf8_lossy(&b).into_owned());
    Some(e)
}

impl FormatHandler for TarHandler {
    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()> {
        let mut archive = Archive::new(open_stream(ctx)?);
        visitor.begin(None);
        for item in archive.entries().map_err(corrupt)? {
            ctx.tracker.check_cancel()?;
            let mut item = item.map_err(corrupt)?;
            let Some(entry) = entry_of(&item) else {
                continue;
            };
            if !visitor.wants(&entry)? {
                continue;
            }
            if entry.kind == EntryKind::File {
                visitor.visit(&entry, Some(&mut item))?;
            } else {
                visitor.visit(&entry, None)?;
            }
        }
        // The tar end marker comes before the compressor's trailer; read the
        // rest so gzip/xz/zstd checksums are actually verified.
        let mut rest = archive.into_inner();
        io::copy(&mut rest, &mut io::sink()).map_err(corrupt)?;
        Ok(())
    }

    fn create(&self, out: &mut File, job: &CreateJob) -> Result<()> {
        let mut builder = new_builder(out, job)?;
        append_inputs(&mut builder, job)?;
        finish(builder, job)
    }

    fn append(&self, ctx: &ReadContext, out: &mut File, job: &CreateJob) -> Result<()> {
        let replaced: HashSet<&str> = job.inputs.iter().map(|i| i.name.as_str()).collect();
        let mut source = Archive::new(open_stream(ctx)?);
        let mut builder = new_builder(out, job)?;
        for item in source.entries().map_err(corrupt)? {
            ctx.tracker.check_cancel()?;
            let mut item = item.map_err(corrupt)?;
            let path = item.path().map_err(corrupt)?.into_owned();
            let name = String::from_utf8_lossy(&item.path_bytes()).into_owned();
            if replaced.contains(name.trim_end_matches('/')) {
                continue;
            }
            let mut header = item.header().clone();
            match header.entry_type() {
                EntryType::XGlobalHeader | EntryType::XHeader | EntryType::GNULongName | EntryType::GNULongLink => {}
                EntryType::Symlink | EntryType::Link => {
                    let target = item.link_name().map_err(corrupt)?.unwrap_or_default().into_owned();
                    builder.append_link(&mut header, &path, &target).map_err(|e| job.map_err(e, ctx.path))?;
                }
                _ => builder.append_data(&mut header, &path, &mut item).map_err(|e| job.map_err(e, ctx.path))?,
            }
        }
        append_inputs(&mut builder, job)?;
        finish(builder, job)
    }
}

fn new_builder<'w>(out: &'w mut File, job: &CreateJob) -> Result<Builder<Box<dyn FinishWrite + 'w>>> {
    let writer = BufWriter::with_capacity(1 << 20, out);
    let encoder: Box<dyn FinishWrite + 'w> = match job.format.codec() {
        Some(codec) => codec.encoder(writer, job.level.unwrap_or(0), job.threads)?,
        None => Box::new(Plain(writer)),
    };
    let mut builder = Builder::new(encoder);
    builder.follow_symlinks(false);
    Ok(builder)
}

fn finish(builder: Builder<Box<dyn FinishWrite + '_>>, job: &CreateJob) -> Result<()> {
    let encoder = builder.into_inner().map_err(|e| job.map_err(e, std::path::Path::new("archive")))?;
    encoder.finish()?;
    Ok(())
}

fn append_inputs<W: Write>(builder: &mut Builder<W>, job: &CreateJob) -> Result<()> {
    for item in &job.inputs {
        job.tracker.check_cancel()?;
        job.tracker.start_entry(&item.name);
        let mut header = Header::new_gnu();
        let mtime = item.modified.and_then(system_time_to_unix).unwrap_or(0).max(0);
        header.set_mtime(mtime as u64);
        // Owner ids are left at 0 so archives don't leak local account details.
        header.set_uid(0);
        header.set_gid(0);
        let err = |e| job.map_err(e, &item.source);
        match &item.kind {
            InputKind::File { size } => {
                header.set_entry_type(EntryType::Regular);
                header.set_mode(item.mode.unwrap_or(0o644));
                header.set_size(*size);
                let reader = job.open(item)?;
                builder.append_data(&mut header, &item.name, reader).map_err(err)?;
            }
            InputKind::Directory => {
                header.set_entry_type(EntryType::Directory);
                header.set_mode(item.mode.unwrap_or(0o755));
                header.set_size(0);
                builder.append_data(&mut header, format!("{}/", item.name), io::empty()).map_err(err)?;
            }
            InputKind::Symlink { target } => {
                header.set_entry_type(EntryType::Symlink);
                header.set_mode(0o777);
                header.set_size(0);
                builder.append_link(&mut header, &item.name, target).map_err(err)?;
            }
        }
        job.tracker.finish_entry();
    }
    Ok(())
}
