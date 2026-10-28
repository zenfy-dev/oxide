//! Single-file compressed streams: `.gz`, `.bz2`, `.xz`, `.zst`.
//!
//! These hold exactly one file whose name is derived from the archive name
//! (`notes.txt.gz` → `notes.txt`). Embedded gzip file names are ignored.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};

use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::{Codec, CreateJob};
use crate::error::{IoContext, OxideError, Result};
use crate::extraction::system_time_to_unix;
use crate::progress::CountingReader;

pub(crate) struct StreamHandler;

fn codec(ctx: &ReadContext) -> Result<Codec> {
    ctx.format.codec().ok_or_else(|| OxideError::InvalidOption(format!("{} is not a stream format", ctx.format)))
}

fn entry_for(ctx: &ReadContext) -> Entry {
    let file_name = ctx.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = match file_name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => "data".to_owned(),
    };
    let mut e = Entry::new(stem, EntryKind::File);
    if let Ok(meta) = fs::metadata(ctx.path) {
        e.compressed_size = Some(meta.len());
        e.modified = meta.modified().ok().and_then(system_time_to_unix);
    }
    e
}

impl FormatHandler for StreamHandler {
    fn list(&self, ctx: &ReadContext) -> Result<Vec<Entry>> {
        codec(ctx)?;
        Ok(vec![entry_for(ctx)])
    }

    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()> {
        let codec = codec(ctx)?;
        let entry = entry_for(ctx);
        visitor.begin(Some(std::slice::from_ref(&entry)));
        if !visitor.wants(&entry)? {
            return Ok(());
        }
        let file = File::open(ctx.path).at(ctx.path)?;
        let input = BufReader::with_capacity(256 * 1024, CountingReader::new(file, ctx.tracker));
        let mut decoder = codec.decoder(input)?;
        visitor.visit(&entry, Some(&mut decoder))
    }

    fn create(&self, out: &mut File, job: &CreateJob) -> Result<()> {
        let codec = job
            .format
            .codec()
            .ok_or_else(|| OxideError::InvalidOption(format!("{} is not a stream format", job.format)))?;
        let item = &job.inputs[0];
        job.tracker.start_entry(&item.name);
        let mut encoder = codec.encoder(BufWriter::with_capacity(1 << 20, out), job.level.unwrap_or(0), job.threads)?;
        job.copy_into(item, &mut encoder)?;
        encoder.flush()?;
        encoder.finish()?;
        job.tracker.finish_entry();
        Ok(())
    }
}
