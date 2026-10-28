//! Create → list → test → extract round trips for every writable format.

mod common;

use std::fs;

use common::{has_temp_files, make_tree, noise, snapshot};
use oxide_core::{
    ArchiveFormat, CompressionMethod, CreateOptions, EncryptionMethod, EntryKind, ExtractOptions, OxideError, Password,
    Progress, ReadOptions,
};
use tempfile::tempdir;

const MULTI_FILE: &[ArchiveFormat] = &[
    ArchiveFormat::Zip,
    ArchiveFormat::SevenZ,
    ArchiveFormat::Tar,
    ArchiveFormat::TarGz,
    ArchiveFormat::TarBz2,
    ArchiveFormat::TarXz,
    ArchiveFormat::TarZst,
];

fn roundtrip(format: ArchiveFormat, options: CreateOptions, read_pw: Option<&str>) {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let archive = tmp.path().join(format!("out.{}", format.extension()));
    let progress = Progress::none();

    let summary = oxide_core::create(std::slice::from_ref(&src), &archive, &options, &progress)
        .unwrap_or_else(|e| panic!("{format}: create failed: {e}"));
    assert!(summary.files >= 6, "{format}: {summary:?}");
    assert_eq!(oxide_core::detect_format(&archive).unwrap(), format);

    let read = ReadOptions { password: read_pw.map(Password::from), format: None };
    let info = oxide_core::list(&archive, &read).unwrap();
    assert_eq!(info.format, format);
    assert!(info.entries.iter().any(|e| e.path.trim_end_matches('/') == "project/src/deep/er/est/leaf.txt"));
    assert!(
        info.entries.iter().any(|e| e.is_dir() && e.trimmed_path() == "project/empty-dir"),
        "{format}: empty dir missing"
    );
    assert_eq!(info.file_count(), 7, "{format}");

    let tested = oxide_core::test(&archive, &read, &progress).unwrap();
    assert_eq!(tested.files, 7, "{format}");

    let out = tmp.path().join("extracted");
    let opts = ExtractOptions { password: read_pw.map(Password::from), ..Default::default() };
    oxide_core::extract(&archive, &out, &opts, &progress).unwrap_or_else(|e| panic!("{format}: extract failed: {e}"));
    assert_eq!(snapshot(&src), snapshot(&out.join("project")), "{format}: contents differ");
    assert!(!has_temp_files(tmp.path()), "{format}: temp files left behind");
}

#[test]
fn every_multi_file_format_round_trips() {
    for &format in MULTI_FILE {
        roundtrip(format, CreateOptions::default(), None);
    }
}

#[test]
fn zip_methods_round_trip() {
    for method in [CompressionMethod::Store, CompressionMethod::Bzip2, CompressionMethod::Zstd, CompressionMethod::Xz] {
        roundtrip(ArchiveFormat::Zip, CreateOptions { method: Some(method), ..Default::default() }, None);
    }
}

#[test]
fn sevenz_store_and_multithreaded() {
    roundtrip(
        ArchiveFormat::SevenZ,
        CreateOptions { method: Some(CompressionMethod::Store), ..Default::default() },
        None,
    );
    roundtrip(ArchiveFormat::SevenZ, CreateOptions { threads: 4, level: Some(3), ..Default::default() }, None);
}

#[test]
fn compressed_tars_multithreaded_and_single_threaded() {
    for format in [ArchiveFormat::TarZst, ArchiveFormat::TarXz] {
        for threads in [1, 4] {
            roundtrip(format, CreateOptions { threads, ..Default::default() }, None);
        }
    }
}

#[test]
fn zip_aes_encryption() {
    for enc in [EncryptionMethod::Aes256, EncryptionMethod::Aes128] {
        let opts =
            CreateOptions { password: Some("correct horse".into()), encryption: Some(enc), ..Default::default() };
        roundtrip(ArchiveFormat::Zip, opts, Some("correct horse"));
    }
}

#[test]
fn sevenz_encryption_hides_headers() {
    let opts = CreateOptions { password: Some("s3cret".into()), ..Default::default() };
    roundtrip(ArchiveFormat::SevenZ, opts.clone(), Some("s3cret"));

    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let archive = tmp.path().join("enc.7z");
    oxide_core::create(&[src], &archive, &opts, &Progress::none()).unwrap();
    // Header encryption: even listing needs the password.
    let err = oxide_core::list(&archive, &ReadOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::PasswordRequired), "{err:?}");
    let bad = ReadOptions { password: Some("wrong".into()), format: None };
    let err = oxide_core::list(&archive, &bad).unwrap_err();
    assert!(matches!(err, OxideError::WrongPassword | OxideError::Corrupt(_)), "{err:?}");
}

#[test]
fn zip_wrong_or_missing_password() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let archive = tmp.path().join("enc.zip");
    let opts = CreateOptions { password: Some("right".into()), ..Default::default() };
    oxide_core::create(&[src], &archive, &opts, &Progress::none()).unwrap();

    let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
    assert!(info.is_encrypted());

    let out = tmp.path().join("out");
    let err = oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::PasswordRequired), "{err:?}");

    let wrong = ExtractOptions { password: Some("nope".into()), ..Default::default() };
    let err = oxide_core::extract(&archive, &out, &wrong, &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::WrongPassword), "{err:?}");
    assert!(!has_temp_files(tmp.path()));
}

#[test]
fn encryption_rejected_for_tar() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let opts = CreateOptions { password: Some("x".into()), ..Default::default() };
    let err = oxide_core::create(&[src], &tmp.path().join("a.tar.gz"), &opts, &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::Unsupported { .. }), "{err:?}");
}

#[test]
fn single_file_streams_round_trip() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("notes.txt");
    fs::write(&input, "line\n".repeat(10_000)).unwrap();
    for format in [ArchiveFormat::Gzip, ArchiveFormat::Bzip2, ArchiveFormat::Xz, ArchiveFormat::Zstd] {
        let archive = tmp.path().join(format!("notes.txt.{}", format.extension()));
        oxide_core::create(std::slice::from_ref(&input), &archive, &CreateOptions::default(), &Progress::none())
            .unwrap();
        assert_eq!(oxide_core::detect_format(&archive).unwrap(), format);
        let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
        assert_eq!(info.entries[0].path, "notes.txt");
        oxide_core::test(&archive, &ReadOptions::default(), &Progress::none()).unwrap();
        let out = tmp.path().join(format!("out-{}", format.extension()));
        oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
        assert_eq!(fs::read(out.join("notes.txt")).unwrap(), fs::read(&input).unwrap(), "{format}");
    }
}

#[test]
fn single_file_formats_reject_directories() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let err =
        oxide_core::create(&[src], &tmp.path().join("x.gz"), &CreateOptions::default(), &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::InvalidOption(_)), "{err:?}");
}

#[test]
fn compression_levels_matter_and_are_validated() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("text.txt");
    // Pseudo-random prose: compressible, but not trivially periodic.
    const WORDS: &[&str] =
        &["archive", "rust", "oxide", "stream", "level", "byte", "folder", "the", "a", "of", "fast", "safe"];
    let text: Vec<&str> = noise(120_000, 3).iter().map(|b| WORDS[*b as usize % WORDS.len()]).collect();
    fs::write(&input, text.join(" ")).unwrap();
    let size_at = |format: ArchiveFormat, level: i32| {
        let out = tmp.path().join(format!("l{level}.{}", format.extension()));
        let opts = CreateOptions { level: Some(level), overwrite: true, ..Default::default() };
        oxide_core::create(std::slice::from_ref(&input), &out, &opts, &Progress::none()).unwrap();
        fs::metadata(&out).unwrap().len()
    };
    assert!(size_at(ArchiveFormat::Zip, 9) < size_at(ArchiveFormat::Zip, 0));
    assert!(size_at(ArchiveFormat::TarZst, 19) < size_at(ArchiveFormat::TarZst, 1));
    assert!(size_at(ArchiveFormat::TarGz, 9) < size_at(ArchiveFormat::TarGz, 1));

    for (format, bad) in [(ArchiveFormat::Zip, 10), (ArchiveFormat::TarZst, 23), (ArchiveFormat::TarBz2, 0)] {
        let opts = CreateOptions { level: Some(bad), ..Default::default() };
        let err = oxide_core::create(
            std::slice::from_ref(&input),
            &tmp.path().join(format!("bad.{}", format.extension())),
            &opts,
            &Progress::none(),
        )
        .unwrap_err();
        assert!(matches!(err, OxideError::InvalidOption(_)), "{format} level {bad}: {err:?}");
    }
    let opts = CreateOptions { method: Some(CompressionMethod::Store), level: Some(3), ..Default::default() };
    assert!(oxide_core::create(&[input], &tmp.path().join("s.zip"), &opts, &Progress::none()).is_err());
}

#[test]
fn empty_directory_archives() {
    let tmp = tempdir().unwrap();
    let empty_dir = tmp.path().join("nothing");
    fs::create_dir(&empty_dir).unwrap();
    for &format in MULTI_FILE {
        let archive = tmp.path().join(format!("empty.{}", format.extension()));
        oxide_core::create(std::slice::from_ref(&empty_dir), &archive, &CreateOptions::default(), &Progress::none())
            .unwrap_or_else(|e| panic!("{format}: {e}"));
        let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
        assert_eq!(info.file_count(), 0, "{format}");
        assert_eq!(info.dir_count(), 1, "{format}");
        let out = tmp.path().join(format!("out-{}", format.extension()));
        let s = oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
        assert_eq!((s.files, s.directories), (0, 1), "{format}");
        assert!(out.join("nothing").is_dir());
    }
}

#[test]
fn archives_with_no_entries() {
    let tmp = tempdir().unwrap();
    let zip_path = tmp.path().join("none.zip");
    zip::ZipWriter::new(fs::File::create(&zip_path).unwrap()).finish().unwrap();
    let tar_path = tmp.path().join("none.tar");
    tar::Builder::new(fs::File::create(&tar_path).unwrap()).into_inner().unwrap();
    for archive in [zip_path, tar_path] {
        let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
        assert!(info.entries.is_empty());
        assert_eq!(info.compression_ratio(), None);
        oxide_core::test(&archive, &ReadOptions::default(), &Progress::none()).unwrap();
        let out = tmp.path().join("out");
        let s = oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
        assert_eq!(s.files, 0);
    }
}

#[test]
fn large_file_streams() {
    let tmp = tempdir().unwrap();
    let big = tmp.path().join("big.bin");
    let data = noise(24 * 1024 * 1024, 42);
    fs::write(&big, &data).unwrap();
    for format in [ArchiveFormat::Zip, ArchiveFormat::TarZst, ArchiveFormat::SevenZ] {
        let archive = tmp.path().join(format!("big.{}", format.extension()));
        let opts = CreateOptions { level: Some(1), ..Default::default() };
        oxide_core::create(std::slice::from_ref(&big), &archive, &opts, &Progress::none()).unwrap();
        let out = tmp.path().join(format!("o-{}", format.extension()));
        oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
        assert!(fs::read(out.join("big.bin")).unwrap() == data, "{format}");
    }
}

/// Zip64: a > 4 GiB entry. Slow and disk-hungry, so opt-in:
/// `cargo test -p oxide-core --release -- --ignored`.
#[test]
#[ignore]
fn zip64_over_4gib() {
    use std::io::Write;
    let tmp = tempdir().unwrap();
    let big = tmp.path().join("huge.bin");
    let mut f = fs::File::create(&big).unwrap();
    let chunk = vec![7u8; 1 << 20];
    for _ in 0..4200 {
        f.write_all(&chunk).unwrap();
    }
    drop(f);
    let archive = tmp.path().join("huge.zip");
    oxide_core::create(
        std::slice::from_ref(&big),
        &archive,
        &CreateOptions { level: Some(1), ..Default::default() },
        &Progress::none(),
    )
    .unwrap();
    let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
    assert_eq!(info.entries[0].size, Some(4200 << 20));
    oxide_core::test(&archive, &ReadOptions::default(), &Progress::none()).unwrap();
}

#[test]
fn hidden_files_and_flattening() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let archive = tmp.path().join("flat.zip");
    let opts = CreateOptions { include_hidden: false, preserve_paths: false, ..Default::default() };
    oxide_core::create(&[src], &archive, &opts, &Progress::none()).unwrap();
    let names: Vec<String> =
        oxide_core::list(&archive, &ReadOptions::default()).unwrap().entries.into_iter().map(|e| e.path).collect();
    assert!(names.contains(&"leaf.txt".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.contains(".hidden")), "{names:?}");
    assert!(!names.iter().any(|n| n.contains('/')), "{names:?}");
}

#[test]
fn flattening_detects_collisions() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("a")).unwrap();
    fs::create_dir_all(tmp.path().join("b")).unwrap();
    fs::write(tmp.path().join("a/same.txt"), "1").unwrap();
    fs::write(tmp.path().join("b/same.txt"), "2").unwrap();
    let opts = CreateOptions { preserve_paths: false, ..Default::default() };
    let err = oxide_core::create(
        &[tmp.path().join("a"), tmp.path().join("b")],
        &tmp.path().join("c.zip"),
        &opts,
        &Progress::none(),
    )
    .unwrap_err();
    assert!(matches!(err, OxideError::InvalidOption(_)));
}

#[test]
fn archive_inside_its_own_source_excludes_itself() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let archive = src.join("self.zip");
    oxide_core::create(std::slice::from_ref(&src), &archive, &CreateOptions::default(), &Progress::none()).unwrap();
    let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
    assert!(!info.entries.iter().any(|e| e.path.ends_with("self.zip")));
    // Refuses to clobber without overwrite.
    let err = oxide_core::create(&[src], &archive, &CreateOptions::default(), &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::AlreadyExists(_)));
}

#[test]
fn selective_extraction() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    for format in [ArchiveFormat::Zip, ArchiveFormat::TarGz, ArchiveFormat::SevenZ] {
        let archive = tmp.path().join(format!("sel.{}", format.extension()));
        oxide_core::create(std::slice::from_ref(&src), &archive, &CreateOptions::default(), &Progress::none()).unwrap();
        let out = tmp.path().join(format!("sel-{}", format.extension()));
        let opts = ExtractOptions {
            selection: Some(vec!["project/src".into(), "project/README.md".into()]),
            ..Default::default()
        };
        oxide_core::extract(&archive, &out, &opts, &Progress::none()).unwrap();
        assert!(out.join("project/src/deep/er/est/leaf.txt").is_file(), "{format}");
        assert!(out.join("project/README.md").is_file(), "{format}");
        assert!(!out.join("project/noise.bin").exists(), "{format}");
        // A prefix that is not a whole component must not match.
        assert!(!out.join("project/empty-dir").exists(), "{format}");
    }
}

#[test]
fn add_files_to_existing_archives() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let extra = tmp.path().join("extra.txt");
    fs::write(&extra, "added later").unwrap();
    for format in [ArchiveFormat::Zip, ArchiveFormat::TarGz, ArchiveFormat::Tar] {
        let archive = tmp.path().join(format!("edit.{}", format.extension()));
        oxide_core::create(std::slice::from_ref(&src), &archive, &CreateOptions::default(), &Progress::none()).unwrap();
        let before = oxide_core::list(&archive, &ReadOptions::default()).unwrap().entries.len();
        oxide_core::add(&archive, std::slice::from_ref(&extra), &CreateOptions::default(), &Progress::none()).unwrap();
        let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
        assert_eq!(info.entries.len(), before + 1, "{format}");
        // Adding again replaces rather than duplicates.
        fs::write(&extra, "changed").unwrap();
        oxide_core::add(&archive, std::slice::from_ref(&extra), &CreateOptions::default(), &Progress::none()).unwrap();
        assert_eq!(oxide_core::list(&archive, &ReadOptions::default()).unwrap().entries.len(), before + 1);
        let out = tmp.path().join(format!("edit-out-{}", format.extension()));
        oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
        assert_eq!(fs::read_to_string(out.join("extra.txt")).unwrap(), "changed");
        assert_eq!(snapshot(&src), snapshot(&out.join("project")), "{format}");
        fs::write(&extra, "added later").unwrap();
    }
    let sz = tmp.path().join("x.7z");
    oxide_core::create(&[src], &sz, &CreateOptions::default(), &Progress::none()).unwrap();
    assert!(matches!(
        oxide_core::add(&sz, &[extra], &CreateOptions::default(), &Progress::none()),
        Err(OxideError::Unsupported { .. })
    ));
}

#[test]
fn detection_uses_signatures_not_names() {
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let real = tmp.path().join("a.tar.zst");
    oxide_core::create(&[src], &real, &CreateOptions::default(), &Progress::none()).unwrap();
    let disguised = tmp.path().join("download.bin");
    fs::copy(&real, &disguised).unwrap();
    assert_eq!(oxide_core::detect_format(&disguised).unwrap(), ArchiveFormat::TarZst);
    let info = oxide_core::list(&disguised, &ReadOptions::default()).unwrap();
    assert_eq!(info.file_count(), 7);

    let text = tmp.path().join("fake.zip");
    fs::write(&text, "definitely not a zip").unwrap();
    assert!(matches!(oxide_core::detect_format(&text), Err(OxideError::UnknownFormat(_))));
}

#[cfg(unix)]
#[test]
fn symlinks_round_trip() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("links");
    fs::create_dir_all(src.join("dir")).unwrap();
    fs::write(src.join("dir/target.txt"), "t").unwrap();
    std::os::unix::fs::symlink("dir/target.txt", src.join("rel-link")).unwrap();
    std::os::unix::fs::symlink("dir", src.join("dir-link")).unwrap();
    for format in [ArchiveFormat::Zip, ArchiveFormat::TarXz, ArchiveFormat::SevenZ] {
        let archive = tmp.path().join(format!("l.{}", format.extension()));
        oxide_core::create(std::slice::from_ref(&src), &archive, &CreateOptions::default(), &Progress::none()).unwrap();
        let info = oxide_core::list(&archive, &ReadOptions::default()).unwrap();
        assert_eq!(info.entries.iter().filter(|e| e.kind == EntryKind::Symlink).count(), 2, "{format}");
        let out = tmp.path().join(format!("lo-{}", format.extension()));
        oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
        assert_eq!(snapshot(&src), snapshot(&out.join("links")), "{format}");
    }
}

#[test]
fn progress_reports_and_finishes() {
    use std::sync::{Arc, Mutex};
    let tmp = tempdir().unwrap();
    let src = make_tree(tmp.path());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let sink = updates.clone();
    let progress =
        Progress::with_callback(move |u| sink.lock().unwrap().push(u.clone())).throttle(std::time::Duration::ZERO);
    let archive = tmp.path().join("p.zip");
    oxide_core::create(&[src], &archive, &CreateOptions::default(), &progress).unwrap();
    let updates = updates.lock().unwrap();
    let last = updates.last().unwrap();
    assert_eq!(last.fraction, Some(1.0));
    assert_eq!(last.entries_total, Some(last.entries_done));
    assert!(updates.iter().any(|u| u.current.is_some()));
}
