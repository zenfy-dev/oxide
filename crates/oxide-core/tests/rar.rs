//! RAR extraction against the vendored sample archives (feature `rar`).

#![cfg(feature = "rar")]

mod common;

use std::fs;

use common::fixture;
use oxide_core::{ArchiveFormat, ExtractOptions, OxideError, Progress, ReadOptions};
use tempfile::tempdir;

#[test]
fn lists_tests_and_extracts() {
    let archive = fixture("rar/version.rar");
    assert_eq!(oxide_core::detect_format(&archive).unwrap(), ArchiveFormat::Rar);
    let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
    assert_eq!(info.entries.len(), 1);
    assert_eq!(info.entries[0].path, "VERSION");
    oxide_core::test(&archive, &ReadOptions::default(), &Progress::none()).unwrap();

    let tmp = tempdir().unwrap();
    let s = oxide_core::extract(&archive, tmp.path(), &ExtractOptions::default(), &Progress::none()).unwrap();
    assert_eq!(s.files, 1);
    assert!(!fs::read(tmp.path().join("VERSION")).unwrap().is_empty());
}

#[test]
fn solid_and_unicode() {
    for name in ["rar/solid.rar", "rar/unicode.rar"] {
        let tmp = tempdir().unwrap();
        let info = oxide_core::list(&fixture(name), &ReadOptions::default()).unwrap();
        let s = oxide_core::extract(&fixture(name), tmp.path(), &ExtractOptions::default(), &Progress::none()).unwrap();
        assert_eq!(s.files as usize, info.file_count(), "{name}");
    }
}

#[test]
fn encrypted_rar() {
    let archive = fixture("rar/crypted.rar");
    let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
    assert!(info.is_encrypted());

    let tmp = tempdir().unwrap();
    let err = oxide_core::extract(&archive, tmp.path(), &ExtractOptions::default(), &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::PasswordRequired), "{err:?}");

    let opts = ExtractOptions { password: Some("unrar".into()), ..Default::default() };
    oxide_core::extract(&archive, tmp.path(), &opts, &Progress::none()).unwrap();
    assert_eq!(fs::read_to_string(tmp.path().join(".gitignore")).unwrap(), "target\nCargo.lock\n");

    let bad = ExtractOptions { password: Some("wrong".into()), ..Default::default() };
    let err = oxide_core::extract(&archive, &tmp.path().join("bad"), &bad, &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::WrongPassword | OxideError::Corrupt(_)), "{err:?}");
}
