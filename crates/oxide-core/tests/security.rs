//! Hostile and broken archives. Each test builds its archive by hand so the
//! malicious names reach Oxide unmodified.

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;

use common::{has_temp_files, noise};
use oxide_core::{
    CancelToken, CreateOptions, ExtractLimits, ExtractOptions, OverwritePolicy, OxideError, Progress, ReadOptions,
};
use tar::{EntryType, Header};
use tempfile::tempdir;

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

/// Writes a tar whose header names and link targets are copied in raw,
/// bypassing the `tar` crate's own path validation.
fn write_raw_tar(path: &Path, entries: &[(&str, EntryType, &str, &[u8], u32)]) {
    let mut builder = tar::Builder::new(fs::File::create(path).unwrap());
    for (name, kind, link, data, mode) in entries {
        let mut h = Header::new_old();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        h.set_entry_type(*kind);
        h.set_size(data.len() as u64);
        h.set_mode(*mode);
        h.set_cksum();
        builder.append(&h, *data).unwrap();
    }
    builder.into_inner().unwrap();
}

fn extract(archive: &Path, dest: &Path, opts: &ExtractOptions) -> oxide_core::Result<oxide_core::OperationSummary> {
    oxide_core::extract(archive, dest, opts, &Progress::none())
}

#[test]
fn zip_slip_is_rejected() {
    for evil in ["../evil.txt", "a/../../evil.txt", "..\\evil.txt", "safe/..\\..\\evil.txt"] {
        let tmp = tempdir().unwrap();
        let archive = tmp.path().join("evil.zip");
        write_zip(&archive, &[("ok.txt", b"fine"), (evil, b"pwned")]);
        let dest = tmp.path().join("dest");
        let err = extract(&archive, &dest, &ExtractOptions::default()).unwrap_err();
        assert!(matches!(err, OxideError::UnsafePath { .. }), "{evil}: {err:?}");
        assert!(!tmp.path().join("evil.txt").exists(), "{evil} escaped");
        assert!(!has_temp_files(tmp.path()));
    }
}

#[test]
fn stripping_components_cannot_hide_traversal() {
    for name in ["../outside.txt", "wrapper/../outside.txt", "wrapper/dir/../../outside.txt"] {
        let tmp = tempdir().unwrap();
        let archive = tmp.path().join("evil.zip");
        write_zip(&archive, &[(name, b"pwned")]);
        for strip_components in [1, 2, usize::MAX] {
            let opts = ExtractOptions { strip_components, ..Default::default() };
            let err = extract(&archive, &tmp.path().join("out"), &opts).unwrap_err();
            assert!(matches!(err, OxideError::UnsafePath { .. }), "{name}: {err:?}");
        }
        assert!(!tmp.path().join("outside.txt").exists());
    }
}

#[test]
fn exact_selection_does_not_extract_descendants_or_normalized_aliases() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("single.zip");
    write_zip(
        &archive,
        &[
            ("report.txt", b"selected"),
            ("report.txt/../../outside.txt", b"must never be decoded"),
            ("./report.txt", b"different stored name"),
            ("other.txt", b"unselected"),
        ],
    );
    let out = tmp.path().join("out");
    let options =
        ExtractOptions { selection: Some(vec!["report.txt".into()]), exact_selection: true, ..Default::default() };
    let summary = extract(&archive, &out, &options).unwrap();
    assert_eq!(fs::read(out.join("report.txt")).unwrap(), b"selected");
    assert_eq!(summary.files, 1);
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
    assert!(!tmp.path().join("outside.txt").exists());
}

#[test]
fn stripping_paths_applies_overwrite_policy_to_collisions() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("collision.zip");
    write_zip(&archive, &[("first/file.txt", b"one"), ("second/file.txt", b"two")]);
    let out = tmp.path().join("out");
    let opts = ExtractOptions { strip_components: 1, overwrite: OverwritePolicy::Rename, ..Default::default() };
    let summary = extract(&archive, &out, &opts).unwrap();
    assert_eq!(summary.files, 2);
    assert_eq!(fs::read(out.join("file.txt")).unwrap(), b"one");
    assert_eq!(fs::read(out.join("file (1).txt")).unwrap(), b"two");
    assert!(!has_temp_files(tmp.path()));
}

#[test]
fn hardlink_targets_follow_stripped_paths() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("links.tar");
    write_raw_tar(
        &archive,
        &[
            ("wrapper/data.txt", EntryType::Regular, "", b"payload", 0o644),
            ("wrapper/copy.txt", EntryType::Link, "wrapper/data.txt", b"", 0o644),
            ("wrapper/missing.txt", EntryType::Link, "wrapper", b"", 0o644),
        ],
    );
    let out = tmp.path().join("out");
    let opts = ExtractOptions { strip_components: 1, ..Default::default() };
    let summary = extract(&archive, &out, &opts).unwrap();
    assert_eq!(fs::read(out.join("copy.txt")).unwrap(), b"payload");
    assert_eq!(summary.links, 1);
    assert_eq!(summary.skipped, 1);
    assert!(!out.join("wrapper").exists());
}

#[cfg(unix)]
#[test]
fn symlinks_remain_confined_after_stripping() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("links.tar");
    write_raw_tar(
        &archive,
        &[
            ("wrapper/data.txt", EntryType::Regular, "", b"payload", 0o644),
            ("wrapper/link.txt", EntryType::Symlink, "data.txt", b"", 0o777),
            ("wrapper/escape", EntryType::Symlink, "../outside", b"", 0o777),
        ],
    );
    let out = tmp.path().join("out");
    let opts = ExtractOptions { strip_components: 1, ..Default::default() };
    let summary = extract(&archive, &out, &opts).unwrap();
    assert_eq!(fs::read(out.join("link.txt")).unwrap(), b"payload");
    assert!(fs::symlink_metadata(out.join("escape")).is_err());
    assert_eq!(summary.links, 1);
    assert_eq!(summary.skipped, 1);
}

#[test]
fn tar_traversal_is_rejected() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("evil.tar");
    write_raw_tar(&archive, &[("../../evil.txt", EntryType::Regular, "", b"pwned", 0o644)]);
    let err = extract(&archive, &tmp.path().join("a/b/dest"), &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::UnsafePath { .. }), "{err:?}");
    assert!(!tmp.path().join("a/evil.txt").exists());
    assert!(!tmp.path().join("evil.txt").exists());
}

#[test]
fn absolute_paths_are_confined() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("abs.zip");
    let outside = tmp.path().join("abs-target.txt");
    let abs_name = outside.to_string_lossy().replace('\\', "/");
    write_zip(&archive, &[(&abs_name, b"x"), ("C:\\Windows\\system.ini", b"y")]);
    let dest = tmp.path().join("dest");
    let summary = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert!(!outside.exists(), "absolute path was honoured");
    assert_eq!(summary.files, 2);
    assert!(dest.join("Windows/system.ini").is_file());
    assert!(summary.warnings.iter().any(|w| w.contains("leading")), "{:?}", summary.warnings);
}

#[test]
fn escaping_symlinks_are_not_created() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("links.tar");
    write_raw_tar(
        &archive,
        &[
            ("escape", EntryType::Symlink, "../../outside", b"", 0o777),
            ("abs", EntryType::Symlink, "/etc", b"", 0o777),
            ("escape/pwned.txt", EntryType::Regular, "", b"data", 0o644),
        ],
    );
    let dest = tmp.path().join("dest");
    let summary = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert!(!tmp.path().join("outside").exists());
    assert!(fs::symlink_metadata(dest.join("abs")).is_err(), "absolute symlink created");
    // The file landed in a real directory, not through a link.
    let meta = fs::symlink_metadata(dest.join("escape")).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink());
    assert_eq!(summary.warnings.iter().filter(|w| w.contains("outside the destination")).count(), 2);
}

#[cfg(unix)]
#[test]
fn symlink_chains_cannot_escape() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("chain.tar");
    // Each link is contained lexically, but together they resolve to the parent.
    write_raw_tar(
        &archive,
        &[
            ("d/", EntryType::Directory, "", b"", 0o755),
            ("d/l", EntryType::Symlink, "..", b"", 0o777),
            ("x", EntryType::Symlink, "d/l/..", b"", 0o777),
        ],
    );
    let dest = tmp.path().join("dest");
    let err = extract(&archive, &dest, &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::UnsafePath { .. }), "{err:?}");
    assert!(fs::symlink_metadata(dest.join("x")).is_err(), "escaping link left behind");
}

#[cfg(unix)]
#[test]
fn never_writes_through_existing_symlinks() {
    let tmp = tempdir().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let dest = tmp.path().join("dest");
    fs::create_dir(&dest).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("pre")).unwrap();
    let archive = tmp.path().join("a.zip");
    write_zip(&archive, &[("pre/x.txt", b"pwned")]);
    let err = extract(&archive, &dest, &ExtractOptions { overwrite: OverwritePolicy::Overwrite, ..Default::default() })
        .unwrap_err();
    assert!(matches!(err, OxideError::UnsafePath { .. }), "{err:?}");
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

#[test]
fn hardlinks_are_confined() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("hard.tar");
    write_raw_tar(&archive, &[("h", EntryType::Link, "../../secret", b"", 0o644)]);
    let err = extract(&archive, &tmp.path().join("dest"), &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::UnsafePath { .. }), "{err:?}");

    // An absolute target is re-rooted inside the destination, where it doesn't exist.
    let archive = tmp.path().join("hard2.tar");
    write_raw_tar(&archive, &[("passwd", EntryType::Link, "/etc/passwd", b"", 0o644)]);
    let dest = tmp.path().join("dest2");
    let summary = extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert!(!dest.join("passwd").exists(), "hard link reached outside the destination");
    assert_eq!(summary.skipped, 1);

    // A legitimate link to an earlier entry works.
    let archive = tmp.path().join("hard3.tar");
    write_raw_tar(
        &archive,
        &[("a.txt", EntryType::Regular, "", b"same", 0o644), ("b.txt", EntryType::Link, "a.txt", b"", 0o644)],
    );
    let dest = tmp.path().join("dest3");
    extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"same");
}

#[cfg(unix)]
#[test]
fn setuid_bits_are_dropped() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("suid.tar");
    write_raw_tar(&archive, &[("tool", EntryType::Regular, "", b"#!/bin/sh\n", 0o6755)]);
    let dest = tmp.path().join("dest");
    extract(&archive, &dest, &ExtractOptions::default()).unwrap();
    let mode = fs::metadata(dest.join("tool")).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o755);
}

fn zeros_archive(dir: &Path, name: &str, mib: usize) -> std::path::PathBuf {
    let input = dir.join("zeros.bin");
    let mut f = fs::File::create(&input).unwrap();
    let chunk = vec![0u8; 1 << 20];
    for _ in 0..mib {
        f.write_all(&chunk).unwrap();
    }
    drop(f);
    let archive = dir.join(name);
    oxide_core::create(std::slice::from_ref(&input), &archive, &CreateOptions::default(), &Progress::none()).unwrap();
    fs::remove_file(input).unwrap();
    archive
}

#[test]
fn total_size_limit_stops_bombs() {
    let tmp = tempdir().unwrap();
    // A streaming format, so the declared size can't be checked up front.
    let archive = zeros_archive(tmp.path(), "bomb.tar.zst", 40);
    let dest = tmp.path().join("dest");
    let limits = ExtractLimits { max_total_bytes: Some(8 << 20), ..ExtractLimits::default() };
    let err = extract(&archive, &dest, &ExtractOptions { limits, ..Default::default() }).unwrap_err();
    assert!(matches!(err, OxideError::LimitExceeded(_)), "{err:?}");
    assert!(!dest.join("zeros.bin").exists(), "partial file left under its final name");
    assert!(!has_temp_files(tmp.path()));
}

#[test]
fn default_ratio_limit_stops_bombs() {
    let tmp = tempdir().unwrap();
    let archive = zeros_archive(tmp.path(), "bomb.tar.zst", 80);
    assert!(fs::metadata(&archive).unwrap().len() < 80 * 1024, "test archive not compressed enough");
    let err = extract(&archive, &tmp.path().join("dest"), &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::LimitExceeded(ref m) if m.contains("ratio")), "{err:?}");
    // Explicitly unlimited extraction succeeds.
    let opts = ExtractOptions { limits: ExtractLimits::unlimited(), ..Default::default() };
    extract(&archive, &tmp.path().join("dest2"), &opts).unwrap();
}

#[test]
fn declared_sizes_fail_fast() {
    let tmp = tempdir().unwrap();
    let archive = zeros_archive(tmp.path(), "big.zip", 4);
    let limits = ExtractLimits { max_entry_bytes: Some(1 << 20), ..ExtractLimits::default() };
    let err =
        extract(&archive, &tmp.path().join("dest"), &ExtractOptions { limits, ..Default::default() }).unwrap_err();
    assert!(matches!(err, OxideError::LimitExceeded(ref m) if m.contains("declares")), "{err:?}");
}

#[test]
fn entry_count_limit() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("many.zip");
    let names: Vec<String> = (0..50).map(|i| format!("f{i}.txt")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
    write_zip(&archive, &entries);
    let limits = ExtractLimits { max_entries: Some(10), ..ExtractLimits::default() };
    let err =
        extract(&archive, &tmp.path().join("dest"), &ExtractOptions { limits, ..Default::default() }).unwrap_err();
    assert!(matches!(err, OxideError::LimitExceeded(_)), "{err:?}");
}

#[test]
fn overwrite_policies() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("a.zip");
    write_zip(&archive, &[("file.txt", b"new")]);
    let dest = tmp.path().join("dest");
    fs::create_dir(&dest).unwrap();
    let reset = || fs::write(dest.join("file.txt"), "old").unwrap();

    reset();
    let err = extract(&archive, &dest, &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::AlreadyExists(_)));
    assert_eq!(fs::read_to_string(dest.join("file.txt")).unwrap(), "old");

    let s =
        extract(&archive, &dest, &ExtractOptions { overwrite: OverwritePolicy::Skip, ..Default::default() }).unwrap();
    assert_eq!((s.files, s.skipped), (0, 1));
    assert_eq!(fs::read_to_string(dest.join("file.txt")).unwrap(), "old");

    extract(&archive, &dest, &ExtractOptions { overwrite: OverwritePolicy::Rename, ..Default::default() }).unwrap();
    extract(&archive, &dest, &ExtractOptions { overwrite: OverwritePolicy::Rename, ..Default::default() }).unwrap();
    assert_eq!(fs::read_to_string(dest.join("file.txt")).unwrap(), "old");
    assert_eq!(fs::read_to_string(dest.join("file (1).txt")).unwrap(), "new");
    assert_eq!(fs::read_to_string(dest.join("file (2).txt")).unwrap(), "new");

    extract(&archive, &dest, &ExtractOptions { overwrite: OverwritePolicy::Overwrite, ..Default::default() }).unwrap();
    assert_eq!(fs::read_to_string(dest.join("file.txt")).unwrap(), "new");
    assert!(!has_temp_files(&dest));
}

#[test]
fn corrupted_zip_fails_cleanly() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("data.bin");
    fs::write(&input, noise(200_000, 9)).unwrap();
    let archive = tmp.path().join("c.zip");
    oxide_core::create(&[input], &archive, &CreateOptions::default(), &Progress::none()).unwrap();
    let mut bytes = fs::read(&archive).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 64] {
        *b ^= 0xA5;
    }
    fs::write(&archive, &bytes).unwrap();

    let err = oxide_core::test(&archive, &ReadOptions::default(), &Progress::none()).unwrap_err();
    assert!(matches!(err, OxideError::Corrupt(_)), "{err:?}");
    let dest = tmp.path().join("dest");
    let err = extract(&archive, &dest, &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, OxideError::Corrupt(_)), "{err:?}");
    assert!(!dest.join("data.bin").exists(), "corrupt output kept under the final name");
    assert!(!has_temp_files(tmp.path()));
}

#[test]
fn truncated_and_garbage_archives() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("data.bin");
    fs::write(&input, noise(300_000, 1)).unwrap();
    for ext in ["tar.gz", "tar.xz", "tar.zst", "tar.bz2", "zip", "7z", "gz"] {
        let archive = tmp.path().join(format!("t.{ext}"));
        oxide_core::create(std::slice::from_ref(&input), &archive, &CreateOptions::default(), &Progress::none())
            .unwrap();
        let bytes = fs::read(&archive).unwrap();
        fs::write(&archive, &bytes[..bytes.len() * 2 / 3]).unwrap();
        let err = oxide_core::test(&archive, &ReadOptions::default(), &Progress::none()).unwrap_err();
        assert!(matches!(err, OxideError::Corrupt(_)), "{ext}: {err:?}");
    }
    let garbage = tmp.path().join("g.zip");
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.extend(noise(4096, 5));
    fs::write(&garbage, bytes).unwrap();
    assert!(oxide_core::list(&garbage, &ReadOptions::default()).is_err());
}

#[test]
fn cancellation_leaves_nothing_behind() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("data.bin");
    fs::write(&input, noise(8 << 20, 11)).unwrap();

    // Cancelled before starting.
    let token = CancelToken::new();
    token.cancel();
    let archive = tmp.path().join("c.zip");
    let err = oxide_core::create(
        std::slice::from_ref(&input),
        &archive,
        &CreateOptions::default(),
        &Progress::none().cancel_with(token),
    )
    .unwrap_err();
    assert!(matches!(err, OxideError::Cancelled), "{err:?}");
    assert!(!archive.exists());
    assert!(!has_temp_files(tmp.path()));

    // Cancelled mid-way from the progress callback, for create and extract.
    for name in ["c.tar.xz", "c.7z", "c.zip"] {
        let archive = tmp.path().join(name);
        let token = CancelToken::new();
        let t = token.clone();
        let progress = Progress::with_callback(move |u| {
            if u.bytes_done > 0 {
                t.cancel();
            }
        })
        .cancel_with(token)
        .throttle(std::time::Duration::ZERO);
        let err = oxide_core::create(std::slice::from_ref(&input), &archive, &CreateOptions::default(), &progress)
            .unwrap_err();
        assert!(matches!(err, OxideError::Cancelled), "{name}: {err:?}");
        assert!(!archive.exists(), "{name}");
        assert!(!has_temp_files(tmp.path()), "{name}");
    }

    let archive = tmp.path().join("ok.tar.gz");
    oxide_core::create(&[input], &archive, &CreateOptions::default(), &Progress::none()).unwrap();
    let token = CancelToken::new();
    let t = token.clone();
    let progress = Progress::with_callback(move |u| {
        if u.bytes_done > 0 {
            t.cancel();
        }
    })
    .cancel_with(token)
    .throttle(std::time::Duration::ZERO);
    let dest = tmp.path().join("dest");
    let err = oxide_core::extract(&archive, &dest, &ExtractOptions::default(), &progress).unwrap_err();
    assert!(matches!(err, OxideError::Cancelled), "{err:?}");
    assert!(!dest.join("data.bin").exists());
    assert!(!has_temp_files(tmp.path()));
}

#[test]
fn listing_streams_reports_progress_and_cancels() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("data.bin");
    fs::write(&input, noise(6 << 20, 21)).unwrap();
    let archive = tmp.path().join("l.tar.bz2");
    oxide_core::create(&[input], &archive, &CreateOptions { level: Some(1), ..Default::default() }, &Progress::none())
        .unwrap();

    let saw_fraction = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = saw_fraction.clone();
    let progress = Progress::with_callback(move |u| {
        if u.fraction.is_some_and(|f| f > 0.0 && f < 1.0) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .throttle(std::time::Duration::ZERO);
    let info = oxide_core::list_with_progress(&archive, &ReadOptions::default(), &progress).unwrap();
    assert_eq!(info.file_count(), 1);
    assert!(saw_fraction.load(std::sync::atomic::Ordering::Relaxed), "no intermediate progress while listing");

    // Cancelling while skipping over the single large entry.
    let token = CancelToken::new();
    let t = token.clone();
    let progress = Progress::with_callback(move |u| {
        if u.fraction.is_some_and(|f| f > 0.1) {
            t.cancel();
        }
    })
    .cancel_with(token)
    .throttle(std::time::Duration::ZERO);
    let err = oxide_core::list_with_progress(&archive, &ReadOptions::default(), &progress).unwrap_err();
    assert!(matches!(err, OxideError::Cancelled), "{err:?}");
}
