//! Error types shared by every archive operation.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Convenience alias used throughout the crate.
pub type Result<T, E = OxideError> = std::result::Result<T, E>;

/// Everything that can go wrong while inspecting, creating, or extracting an archive.
///
/// Variants are deliberately coarse so frontends can map them to user-facing
/// messages and process exit codes without knowing format internals.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OxideError {
    /// An operating-system level I/O failure, optionally tied to a path.
    #[error("{}", io_message(path.as_deref(), source))]
    Io {
        /// The file or directory involved, when known.
        path: Option<PathBuf>,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The file's signature and extension did not match any known format.
    #[error("unrecognized archive format: {}", .0.display())]
    UnknownFormat(PathBuf),

    /// The format is known but the requested operation is not available for it.
    #[error("{format} does not support {operation}")]
    Unsupported {
        /// Human-readable format name.
        format: &'static str,
        /// What was attempted, e.g. "creation" or "encryption".
        operation: &'static str,
    },

    /// Support for this format exists but was disabled at compile time.
    #[error("{format} support is not included in this build (enable the `{feature}` Cargo feature)")]
    FeatureDisabled {
        /// Human-readable format name.
        format: &'static str,
        /// The Cargo feature that enables it.
        feature: &'static str,
    },

    /// The archive is malformed, truncated, or failed a checksum.
    #[error("archive is corrupt or malformed: {0}")]
    Corrupt(String),

    /// An entry tried to escape the extraction directory or was otherwise unsafe.
    #[error("unsafe entry rejected: {path:?} ({reason})")]
    UnsafePath {
        /// The entry name exactly as stored in the archive.
        path: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A configured safety limit (size, entry count, ratio) was exceeded.
    #[error("extraction limit exceeded: {0}")]
    LimitExceeded(String),

    /// A destination already exists and the overwrite policy forbids replacing it.
    #[error("destination already exists: {}", .0.display())]
    AlreadyExists(PathBuf),

    /// The archive (or an entry in it) is encrypted and no password was given.
    #[error("this archive is encrypted; a password is required")]
    PasswordRequired,

    /// The supplied password was rejected.
    #[error("incorrect password (or the encrypted data is corrupt)")]
    WrongPassword,

    /// The operation was cancelled through its [`CancelToken`](crate::CancelToken).
    #[error("operation cancelled")]
    Cancelled,

    /// The caller supplied an invalid combination of options.
    #[error("invalid option: {0}")]
    InvalidOption(String),
}

fn io_message(path: Option<&Path>, source: &io::Error) -> String {
    match path {
        Some(p) => format!("{}: {source}", p.display()),
        None => source.to_string(),
    }
}

impl From<io::Error> for OxideError {
    fn from(source: io::Error) -> Self {
        OxideError::Io { path: None, source }
    }
}

impl OxideError {
    /// Wraps an I/O error together with the path it concerns.
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        OxideError::Io { path: Some(path.into()), source }
    }

    /// Classifies a read-side I/O error coming out of a decoder: data errors
    /// become [`OxideError::Corrupt`], everything else stays an I/O error.
    pub(crate) fn from_decode(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidInput => {
                OxideError::Corrupt(err.to_string())
            }
            _ => OxideError::from(err),
        }
    }
}

/// Attaches a path to I/O errors.
pub(crate) trait IoContext<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|e| OxideError::io(path, e))
    }
}
