//! RAR extraction via the `unrar` crate, which bundles RARLAB's UnRAR
//! source. Only compiled with the `rar` feature because the UnRAR licence
//! is freeware rather than OSI open source. RAR creation is not possible.
//!
//! UnRAR only extracts to paths, not to streams, so each file is first
//! written to a private staging directory and then passed through the
//! normal [`ExtractVisitor`](crate::extraction) checks. A staged file that
//! turns out to be a symbolic link is never followed.

use std::fs::{self, File};
use std::path::Path;

use unrar::Archive;
use unrar::error::{Code, UnrarError};
use unrar::{CursorBeforeHeader, OpenArchive, Process};

use super::dos_to_unix;
use crate::archive::{Entry, EntryKind, EntryVisitor, FormatHandler, ReadContext};
use crate::compression::CreateJob;
use crate::error::{IoContext, OxideError, Result};

pub(crate) struct RarHandler;

fn map_err(err: UnrarError) -> OxideError {
    match err.code {
        Code::MissingPassword => OxideError::PasswordRequired,
        Code::BadPassword => OxideError::WrongPassword,
        Code::EOpen | Code::ERead | Code::EWrite | Code::ECreate | Code::EClose | Code::NoMemory => {
            OxideError::from(std::io::Error::other(err.to_string()))
        }
        _ => OxideError::Corrupt(err.to_string()),
    }
}

fn entry_of(h: &unrar::FileHeader) -> Entry {
    let kind = if h.is_directory() { EntryKind::Directory } else { EntryKind::File };
    let mut e = Entry::new(h.filename.to_string_lossy(), kind);
    e.size = Some(h.unpacked_size);
    e.modified = ::zip::DateTime::try_from_msdos((h.file_time >> 16) as u16, (h.file_time & 0xFFFF) as u16)
        .ok()
        .and_then(dos_to_unix);
    e.encrypted = h.is_encrypted();
    e
}

fn archive<'a>(path: &'a Path, password: Option<&'a [u8]>) -> Archive<'a> {
    match password {
        Some(pw) => Archive::with_password(path, pw),
        None => Archive::new(path),
    }
}

impl FormatHandler for RarHandler {
    fn list(&self, ctx: &ReadContext) -> Result<Vec<Entry>> {
        let pw = ctx.password.map(|p| p.expose().as_bytes());
        let listing = archive(ctx.path, pw).open_for_listing().map_err(map_err)?;
        listing.map(|h| h.map(|h| entry_of(&h)).map_err(map_err)).collect()
    }

    fn read(&self, ctx: &ReadContext, visitor: &mut dyn EntryVisitor) -> Result<()> {
        let index = self.list(ctx)?;
        visitor.begin(Some(&index));
        let pw = ctx.password.map(|p| p.expose().as_bytes());
        let staging = tempfile::Builder::new().prefix(".oxide-rar-").tempdir()?;
        let mut cursor: Option<OpenArchive<Process, CursorBeforeHeader>> =
            Some(archive(ctx.path, pw).open_for_processing().map_err(map_err)?);
        let mut counter = 0u64;
        while let Some(open) = cursor.take() {
            ctx.tracker.check_cancel()?;
            let Some(header) = open.read_header().map_err(map_err)? else {
                break;
            };
            let entry = entry_of(header.entry());
            if !visitor.wants(&entry)? {
                cursor = Some(header.skip().map_err(map_err)?);
                continue;
            }
            if entry.is_dir() {
                visitor.visit(&entry, None)?;
                cursor = Some(header.skip().map_err(map_err)?);
                continue;
            }
            if !visitor.needs_data() {
                cursor = Some(header.test().map_err(map_err)?);
                visitor.visit(&entry, None)?;
                continue;
            }
            counter += 1;
            let staged = staging.path().join(counter.to_string());
            cursor = Some(header.extract_to(&staged).map_err(map_err)?);
            let meta = fs::symlink_metadata(&staged).at(&staged)?;
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&staged).at(&staged)?;
                let mut link = entry.clone();
                link.kind = EntryKind::Symlink;
                link.link_target = Some(target.to_string_lossy().into_owned());
                visitor.visit(&link, None)?;
            } else if meta.is_file() {
                let mut file = File::open(&staged).at(&staged)?;
                visitor.visit(&entry, Some(&mut file))?;
            } else {
                let mut other = entry.clone();
                other.kind = EntryKind::Other;
                visitor.visit(&other, None)?;
            }
            let _ = fs::remove_file(&staged);
        }
        Ok(())
    }

    fn create(&self, _out: &mut File, _job: &CreateJob) -> Result<()> {
        Err(OxideError::Unsupported { format: "RAR", operation: "creation" })
    }
}
