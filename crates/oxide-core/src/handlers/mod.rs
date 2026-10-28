//! One module per archive format, each implementing [`FormatHandler`].
//!
//! To add a format: add a variant to [`ArchiveFormat`], give it a signature
//! and capabilities in `formats.rs`, implement [`FormatHandler`] in a new
//! module here, and register it in [`handler_for`]. See `docs/ARCHITECTURE.md`.

use std::time::SystemTime;

use chrono::{Local, TimeZone};

use crate::archive::FormatHandler;
use crate::error::Result;
use crate::formats::ArchiveFormat;

#[cfg(feature = "rar")]
mod rar;
#[cfg(feature = "sevenz")]
mod sevenz;
mod stream;
mod tar;
mod zip;

/// Returns the handler for `format`, or an error if it is not compiled in.
pub(crate) fn handler_for(format: ArchiveFormat) -> Result<&'static dyn FormatHandler> {
    match format {
        ArchiveFormat::Zip => Ok(&zip::ZipHandler),
        ArchiveFormat::Tar
        | ArchiveFormat::TarGz
        | ArchiveFormat::TarBz2
        | ArchiveFormat::TarXz
        | ArchiveFormat::TarZst => Ok(&tar::TarHandler),
        ArchiveFormat::Gzip | ArchiveFormat::Bzip2 | ArchiveFormat::Xz | ArchiveFormat::Zstd => {
            Ok(&stream::StreamHandler)
        }
        #[cfg(feature = "sevenz")]
        ArchiveFormat::SevenZ => Ok(&sevenz::SevenZHandler),
        #[cfg(not(feature = "sevenz"))]
        ArchiveFormat::SevenZ => Err(crate::error::OxideError::FeatureDisabled { format: "7z", feature: "sevenz" }),
        #[cfg(feature = "rar")]
        ArchiveFormat::Rar => Ok(&rar::RarHandler),
        #[cfg(not(feature = "rar"))]
        ArchiveFormat::Rar => Err(crate::error::OxideError::FeatureDisabled { format: "RAR", feature: "rar" }),
    }
}

/// Converts an MS-DOS timestamp (local time, as used by ZIP and RAR) to Unix seconds.
pub(crate) fn dos_to_unix(dt: ::zip::DateTime) -> Option<i64> {
    let naive: chrono::NaiveDateTime = dt.try_into().ok()?;
    Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp())
}

/// Converts a filesystem time to an MS-DOS timestamp in local time.
pub(crate) fn system_to_dos(t: SystemTime) -> Option<::zip::DateTime> {
    let local: chrono::DateTime<Local> = t.into();
    ::zip::DateTime::try_from(local.naive_local()).ok()
}
