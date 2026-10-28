# Format compatibility matrix

Status reflects what has been **implemented and tested**, not what the underlying libraries
advertise. Tests ran on macOS 26 / Apple Silicon with Rust 1.99.1, and are repeated on Linux and
Windows by CI.

| Format | Extract | Create | Encryption | Backend | Automated tests | Checked against other tools |
|---|---|---|---|---|---|---|
| ZIP | ✅ | ✅ Deflate, Store, Bzip2, Zstd, XZ | ✅ AES-256/128 (write); ZipCrypto (read only) | `zip` 9 | round trip for every method, AES, wrong/missing password, Zip Slip, corruption, truncation, add, Zip64 > 4 GiB | Info-ZIP `unzip` reads Oxide ZIPs; Oxide reads Info-ZIP `zip` output |
| 7z | ✅ | ✅ LZMA2 (single- and multi-threaded), Store | ✅ AES-256 + encrypted headers | `sevenz-rust2` 0.24 (pure Rust) | round trip, header encryption, wrong password, symlinks, truncation, cancellation | libarchive (`bsdtar`) reads Oxide 7z; Oxide reads libarchive-written 7z |
| TAR | ✅ | ✅ | — | `tar` 0.4 | round trip, traversal, symlink/hard-link escapes, setuid, add | bsdtar ustar/pax both ways |
| TAR.GZ | ✅ | ✅ levels 0–9 | — | `flate2` | round trip, truncation, multi-member | bsdtar both ways |
| TAR.XZ | ✅ | ✅ levels 0–9, multithreaded | — | `liblzma` (maintained fork of `xz2`) | round trip (1 and 4 threads), truncation | bsdtar both ways; `xz -T4` multi-block streams |
| TAR.ZST | ✅ | ✅ levels 1–22, multithreaded | — | `zstd` | round trip (1 and 4 threads), bombs, truncation | bsdtar both ways; `zstd -T4` |
| TAR.BZ2 | ✅ | ✅ levels 1–9 | — | `bzip2` | round trip, truncation, listing progress/cancel | bsdtar both ways |
| GZIP | ✅ | ✅ single file | — | `flate2` | round trip, rejects folders | `gzip -t`; concatenated members |
| BZIP2 | ✅ | ✅ single file | — | `bzip2` | round trip | `bzip2 -t` |
| XZ | ✅ | ✅ single file | — | `liblzma` | round trip | `xz -t`, `xz -T4` |
| ZSTD | ✅ | ✅ single file | — | `zstd` | round trip | `zstd -t`, `zstd -19 -T4` |
| RAR (v4/v5) | ✅ with `rar` feature | ❌ (not possible) | ✅ read | `unrar` (RARLAB UnRAR) | list/test/extract, solid, Unicode, encrypted (right/wrong/missing password) using sample archives from the `unrar` crate | — |

**Detection** reads file signatures, not names. A `.tar.zst` renamed to `.bin` is still
recognised, and compressed streams are opened to tell `data.gz` apart from a gzipped tarball.
Pre-POSIX tar headers are recognised by checksum.

## Notes from interoperability testing

- libarchive on macOS converts Unicode file names to decomposed form (NFD) when extracting 7z.
  Oxide stores and restores names byte-for-byte.
- macOS `bsdtar` adds AppleDouble `._*` entries unless `COPYFILE_DISABLE=1` is set. Oxide
  extracts them like any other file, as GNU tar would.
- AES-encrypted ZIP and 7z files written by Oxide have not yet been opened with 7-Zip, WinZip
  or Keka. They follow the WinZip AE-2 and 7-Zip AES-256 specifications, and the round trips are
  tested.

## Not supported (yet)

Multi-volume archives of any kind; ISO/CAB/ARJ/LZH; creating RAR (proprietary); editing 7z in
place; preserving extended attributes and ACLs.
