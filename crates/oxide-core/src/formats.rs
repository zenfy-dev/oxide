//! Archive formats, their capabilities, and signature-based detection.

use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::Serialize;

use crate::compression::Codec;
use crate::error::{IoContext, OxideError, Result};

/// Every container or stream format Oxide knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    /// PKZIP archive.
    Zip,
    /// 7-Zip archive.
    #[serde(rename = "7z")]
    SevenZ,
    /// Uncompressed tarball.
    Tar,
    /// Gzip-compressed tarball.
    TarGz,
    /// Bzip2-compressed tarball.
    TarBz2,
    /// XZ-compressed tarball.
    TarXz,
    /// Zstandard-compressed tarball.
    TarZst,
    /// A single gzip-compressed file.
    Gzip,
    /// A single bzip2-compressed file.
    Bzip2,
    /// A single XZ-compressed file.
    Xz,
    /// A single Zstandard-compressed file.
    Zstd,
    /// RAR archive (extraction only).
    Rar,
}

/// Compression algorithms that can be selected when creating archives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompressionMethod {
    /// No compression.
    Store,
    /// DEFLATE (ZIP, gzip).
    Deflate,
    /// bzip2.
    Bzip2,
    /// XZ / LZMA2 in an XZ container.
    Xz,
    /// LZMA2 inside a 7z container.
    Lzma2,
    /// Zstandard.
    Zstd,
}

/// Inclusive compression-level range for a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LevelRange {
    /// Fastest accepted level.
    pub min: i32,
    /// Strongest accepted level.
    pub max: i32,
    /// Level used when none is specified.
    pub default: i32,
}

impl CompressionMethod {
    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            CompressionMethod::Store => "Store",
            CompressionMethod::Deflate => "Deflate",
            CompressionMethod::Bzip2 => "Bzip2",
            CompressionMethod::Xz => "XZ",
            CompressionMethod::Lzma2 => "LZMA2",
            CompressionMethod::Zstd => "Zstandard",
        }
    }

    /// Parses a method name as used on the command line.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "store" | "stored" | "none" | "copy" => CompressionMethod::Store,
            "deflate" | "deflated" | "gzip" => CompressionMethod::Deflate,
            "bzip2" | "bz2" => CompressionMethod::Bzip2,
            "xz" => CompressionMethod::Xz,
            "lzma2" | "lzma" => CompressionMethod::Lzma2,
            "zstd" | "zstandard" | "zst" => CompressionMethod::Zstd,
            _ => return None,
        })
    }

    /// The accepted compression levels, or `None` if the method has no levels.
    pub fn level_range(self) -> Option<LevelRange> {
        let (min, max, default) = match self {
            CompressionMethod::Store => return None,
            CompressionMethod::Deflate => (0, 9, 6),
            CompressionMethod::Bzip2 => (1, 9, 9),
            CompressionMethod::Xz | CompressionMethod::Lzma2 => (0, 9, 6),
            CompressionMethod::Zstd => (1, 22, 3),
        };
        Some(LevelRange { min, max, default })
    }
}

impl fmt::Display for CompressionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a format can do in this particular build.
#[derive(Debug, Clone, Serialize)]
pub struct FormatInfo {
    /// The format.
    pub format: ArchiveFormat,
    /// Display name, e.g. `TAR.ZST`.
    pub name: &'static str,
    /// Recognised file extensions, preferred first.
    pub extensions: &'static [&'static str],
    /// Extraction, listing and testing are available.
    pub can_extract: bool,
    /// Creation is available.
    pub can_create: bool,
    /// The format holds many entries (as opposed to a single compressed stream).
    pub multi_file: bool,
    /// Encryption schemes available when creating.
    pub encryption: &'static [crate::EncryptionMethod],
    /// Selectable compression methods; the first is the default.
    pub methods: &'static [CompressionMethod],
    /// Short note about limitations, if any.
    pub notes: &'static str,
}

impl ArchiveFormat {
    /// All formats, in display order.
    pub const ALL: [ArchiveFormat; 12] = [
        ArchiveFormat::Zip,
        ArchiveFormat::SevenZ,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
        ArchiveFormat::TarBz2,
        ArchiveFormat::Gzip,
        ArchiveFormat::Bzip2,
        ArchiveFormat::Xz,
        ArchiveFormat::Zstd,
        ArchiveFormat::Rar,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "ZIP",
            ArchiveFormat::SevenZ => "7z",
            ArchiveFormat::Tar => "TAR",
            ArchiveFormat::TarGz => "TAR.GZ",
            ArchiveFormat::TarBz2 => "TAR.BZ2",
            ArchiveFormat::TarXz => "TAR.XZ",
            ArchiveFormat::TarZst => "TAR.ZST",
            ArchiveFormat::Gzip => "GZIP",
            ArchiveFormat::Bzip2 => "BZIP2",
            ArchiveFormat::Xz => "XZ",
            ArchiveFormat::Zstd => "ZSTD",
            ArchiveFormat::Rar => "RAR",
        }
    }

    /// Recognised extensions, preferred first (without leading dot).
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            ArchiveFormat::Zip => &["zip"],
            ArchiveFormat::SevenZ => &["7z"],
            ArchiveFormat::Tar => &["tar"],
            ArchiveFormat::TarGz => &["tar.gz", "tgz"],
            ArchiveFormat::TarBz2 => &["tar.bz2", "tbz2", "tbz"],
            ArchiveFormat::TarXz => &["tar.xz", "txz"],
            ArchiveFormat::TarZst => &["tar.zst", "tzst"],
            ArchiveFormat::Gzip => &["gz"],
            ArchiveFormat::Bzip2 => &["bz2"],
            ArchiveFormat::Xz => &["xz"],
            ArchiveFormat::Zstd => &["zst"],
            ArchiveFormat::Rar => &["rar"],
        }
    }

    /// The preferred file extension (without leading dot).
    pub fn extension(self) -> &'static str {
        self.extensions()[0]
    }

    /// The stream codec wrapping this format, if any.
    pub(crate) fn codec(self) -> Option<Codec> {
        match self {
            ArchiveFormat::TarGz | ArchiveFormat::Gzip => Some(Codec::Gzip),
            ArchiveFormat::TarBz2 | ArchiveFormat::Bzip2 => Some(Codec::Bzip2),
            ArchiveFormat::TarXz | ArchiveFormat::Xz => Some(Codec::Xz),
            ArchiveFormat::TarZst | ArchiveFormat::Zstd => Some(Codec::Zstd),
            _ => None,
        }
    }

    /// True for the tar family.
    pub fn is_tar(self) -> bool {
        matches!(
            self,
            ArchiveFormat::Tar
                | ArchiveFormat::TarGz
                | ArchiveFormat::TarBz2
                | ArchiveFormat::TarXz
                | ArchiveFormat::TarZst
        )
    }

    /// Infers the format from a file name's extension alone.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        // Longest extensions first so `.tar.gz` wins over `.gz`.
        let mut best: Option<(usize, ArchiveFormat)> = None;
        for format in Self::ALL {
            for ext in format.extensions() {
                let dotted = format!(".{ext}");
                if name.ends_with(&dotted) && name.len() > dotted.len() && best.is_none_or(|(len, _)| ext.len() > len) {
                    best = Some((ext.len(), format));
                }
            }
        }
        best.map(|(_, f)| f)
    }

    /// Parses a format name or extension, e.g. `zip`, `tar.zst`, `tgz`, `7z`.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim_start_matches('.').to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.extensions().contains(&name.as_str()) || f.name().eq_ignore_ascii_case(&name))
    }

    /// Capabilities of this format in the current build.
    pub fn info(self) -> FormatInfo {
        use crate::EncryptionMethod::{Aes128, Aes256};
        use CompressionMethod as M;
        let (can_extract, can_create, encryption, methods, notes): (
            bool,
            bool,
            &'static [crate::EncryptionMethod],
            &'static [CompressionMethod],
            &'static str,
        ) = match self {
            ArchiveFormat::Zip => (
                true,
                true,
                &[Aes256, Aes128],
                &[M::Deflate, M::Store, M::Bzip2, M::Zstd, M::Xz],
                "Reads legacy ZipCrypto; writes AES only",
            ),
            ArchiveFormat::SevenZ => (
                cfg!(feature = "sevenz"),
                cfg!(feature = "sevenz"),
                if cfg!(feature = "sevenz") { &[Aes256] } else { &[] },
                &[M::Lzma2, M::Store],
                if cfg!(feature = "sevenz") {
                    "Pure-Rust backend (sevenz-rust2)"
                } else {
                    "Disabled in this build (feature `sevenz`)"
                },
            ),
            ArchiveFormat::Tar => (true, true, &[], &[M::Store], ""),
            ArchiveFormat::TarGz | ArchiveFormat::Gzip => (true, true, &[], &[M::Deflate], ""),
            ArchiveFormat::TarBz2 | ArchiveFormat::Bzip2 => (true, true, &[], &[M::Bzip2], ""),
            ArchiveFormat::TarXz | ArchiveFormat::Xz => (true, true, &[], &[M::Xz], ""),
            ArchiveFormat::TarZst | ArchiveFormat::Zstd => (true, true, &[], &[M::Zstd], "Multithreaded compression"),
            ArchiveFormat::Rar => (
                cfg!(feature = "rar"),
                false,
                &[],
                &[],
                if cfg!(feature = "rar") {
                    "Extraction via RARLAB UnRAR (freeware licence)"
                } else {
                    "Disabled in this build (feature `rar`, UnRAR licence)"
                },
            ),
        };
        let notes =
            if matches!(self, ArchiveFormat::Gzip | ArchiveFormat::Bzip2 | ArchiveFormat::Xz | ArchiveFormat::Zstd) {
                "Single file only"
            } else {
                notes
            };
        FormatInfo {
            format: self,
            name: self.name(),
            extensions: self.extensions(),
            can_extract,
            can_create,
            multi_file: !matches!(
                self,
                ArchiveFormat::Gzip | ArchiveFormat::Bzip2 | ArchiveFormat::Xz | ArchiveFormat::Zstd
            ),
            encryption,
            methods,
            notes,
        }
    }

    /// The default compression method for this format.
    pub fn default_method(self) -> CompressionMethod {
        self.info().methods.first().copied().unwrap_or(CompressionMethod::Store)
    }
}

impl fmt::Display for ArchiveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Capabilities of every format in this build.
pub fn supported_formats() -> Vec<FormatInfo> {
    ArchiveFormat::ALL.iter().map(|f| f.info()).collect()
}

/// Identifies a format from the first bytes of a file.
///
/// Compressed streams are reported as their single-file format (`Gzip`,
/// `Xz`, ...); [`detect_format`] additionally peeks inside to recognise tarballs.
pub fn detect_signature(header: &[u8]) -> Option<ArchiveFormat> {
    const SIGNATURES: &[(&[u8], ArchiveFormat)] = &[
        (b"PK\x03\x04", ArchiveFormat::Zip),
        (b"PK\x05\x06", ArchiveFormat::Zip),
        (b"PK\x07\x08", ArchiveFormat::Zip),
        (&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C], ArchiveFormat::SevenZ),
        (b"Rar!\x1A\x07", ArchiveFormat::Rar),
        (&[0x1F, 0x8B], ArchiveFormat::Gzip),
        (b"BZh", ArchiveFormat::Bzip2),
        (&[0xFD, b'7', b'z', b'X', b'Z', 0x00], ArchiveFormat::Xz),
        (&[0x28, 0xB5, 0x2F, 0xFD], ArchiveFormat::Zstd),
    ];
    for (magic, format) in SIGNATURES {
        if header.starts_with(magic) {
            return Some(*format);
        }
    }
    if is_tar_header(header) {
        return Some(ArchiveFormat::Tar);
    }
    None
}

/// Checks for a POSIX/GNU `ustar` magic at offset 257 of a tar header block.
fn is_tar_header(block: &[u8]) -> bool {
    block.len() >= 263 && &block[257..262] == b"ustar"
}

/// Validates the checksum of a pre-POSIX (v7) tar header, which has no magic.
fn is_v7_tar_header(block: &[u8]) -> bool {
    if block.len() < 512 || block[0] == 0 {
        return false;
    }
    let field = &block[148..156];
    let text: String = field.iter().take_while(|b| **b != 0 && **b != b' ').map(|b| *b as char).collect();
    let Ok(stored) = u32::from_str_radix(text.trim(), 8) else {
        return false;
    };
    let sum: u32 =
        block[..512].iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { u32::from(*b) }).sum();
    sum == stored
}

/// Determines an archive's format, preferring file signatures over the name.
///
/// For gzip/bzip2/xz/zstd streams the first decompressed block is inspected so
/// that `data.gz` holding a tarball is still recognised as `TAR.GZ`.
pub fn detect_format(path: &Path) -> Result<ArchiveFormat> {
    let mut file = File::open(path).at(path)?;
    let mut header = [0u8; 512];
    let n = read_up_to(&mut file, &mut header).at(path)?;
    let header = &header[..n];
    let by_name = ArchiveFormat::from_path(path);

    let Some(by_magic) = detect_signature(header) else {
        if is_v7_tar_header(header) {
            return Ok(ArchiveFormat::Tar);
        }
        // Empty or unsigned files: trust the extension only for formats
        // that legitimately lack a signature at offset 0.
        return match by_name {
            Some(f @ ArchiveFormat::Tar) => Ok(f),
            _ => Err(OxideError::UnknownFormat(path.to_path_buf())),
        };
    };

    let Some(codec) = by_magic.codec() else {
        return Ok(by_magic);
    };
    let tar_variant = match codec {
        Codec::Gzip => ArchiveFormat::TarGz,
        Codec::Bzip2 => ArchiveFormat::TarBz2,
        Codec::Xz => ArchiveFormat::TarXz,
        Codec::Zstd => ArchiveFormat::TarZst,
    };
    let file = File::open(path).at(path)?;
    let mut decoder = codec.decoder(BufReader::new(file))?;
    let mut inner = [0u8; 512];
    let inner_len = read_up_to(&mut decoder, &mut inner).unwrap_or(0);
    let inner = &inner[..inner_len];
    if is_tar_header(inner) || is_v7_tar_header(inner) || by_name == Some(tar_variant) {
        Ok(tar_variant)
    } else {
        Ok(by_magic)
    }
}

fn read_up_to(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_prefers_longest_match() {
        assert_eq!(ArchiveFormat::from_path(Path::new("a.tar.gz")), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::from_path(Path::new("a.gz")), Some(ArchiveFormat::Gzip));
        assert_eq!(ArchiveFormat::from_path(Path::new("A.TZST")), Some(ArchiveFormat::TarZst));
        assert_eq!(ArchiveFormat::from_path(Path::new(".zip")), None);
        assert_eq!(ArchiveFormat::from_path(Path::new("notes.txt")), None);
    }

    #[test]
    fn names_parse() {
        assert_eq!(ArchiveFormat::from_name("tar.zst"), Some(ArchiveFormat::TarZst));
        assert_eq!(ArchiveFormat::from_name(".7z"), Some(ArchiveFormat::SevenZ));
        assert_eq!(ArchiveFormat::from_name("TGZ"), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::from_name("bogus"), None);
    }

    #[test]
    fn signatures() {
        assert_eq!(detect_signature(b"PK\x03\x04rest"), Some(ArchiveFormat::Zip));
        assert_eq!(detect_signature(&[0x1F, 0x8B, 8]), Some(ArchiveFormat::Gzip));
        assert_eq!(detect_signature(b"Rar!\x1A\x07\x01\x00"), Some(ArchiveFormat::Rar));
        assert_eq!(detect_signature(b"hello"), None);
    }
}
