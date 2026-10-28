//! # oxide-core
//!
//! The archive engine behind the Oxide CLI and desktop app. It has no
//! knowledge of either frontend and can be embedded in other programs.
//!
//! ```no_run
//! use std::path::{Path, PathBuf};
//! use oxide_core::{CreateOptions, ExtractOptions, Progress};
//!
//! # fn main() -> oxide_core::Result<()> {
//! let progress = Progress::none();
//! oxide_core::create(&[PathBuf::from("docs")], Path::new("docs.tar.zst"), &CreateOptions::default(), &progress)?;
//! let info = oxide_core::list(Path::new("docs.tar.zst"), &Default::default())?;
//! println!("{} files", info.file_count());
//! oxide_core::extract(Path::new("docs.tar.zst"), Path::new("out"), &ExtractOptions::default(), &progress)?;
//! # Ok(()) }
//! ```
//!
//! All operations stream data and never load whole archives into memory.
//! Extraction treats archives as untrusted; see [`extraction`] for the
//! guarantees it provides.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod archive;
pub mod compression;
pub mod encryption;
pub mod error;
pub mod extraction;
pub mod formats;
mod handlers;
pub mod progress;

pub use archive::{ArchiveInfo, Entry, EntryKind, OperationSummary, ReadOptions, add, list, list_with_progress, test};
pub use compression::{CreateOptions, create};
pub use encryption::{EncryptionMethod, Password};
pub use error::{OxideError, Result};
pub use extraction::{ExtractLimits, ExtractOptions, OverwritePolicy, SymlinkPolicy, extract, sanitize_entry_path};
pub use formats::{ArchiveFormat, CompressionMethod, FormatInfo, LevelRange, detect_format, supported_formats};
pub use progress::{CancelToken, Operation, Progress, ProgressUpdate};
