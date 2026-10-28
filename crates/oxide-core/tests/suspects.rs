//! Reproductions for suspected bugs found in review.

mod common;

use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};

use oxide_core::{CreateOptions, ExtractOptions, OverwritePolicy, Progress};
use tar::{EntryType, Header};
use tempfile::tempdir;

#[test]
fn hardlink_to_missing_target_creates_no_folders() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("h.tar");
    let mut b = tar::Builder::new(fs::File::create(&archive).unwrap());
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Link);
    h.set_size(0);
    b.append_link(&mut h, "link", "missing/deep/target").unwrap();
    b.into_inner().unwrap();
    let dest = tmp.path().join("out");
    oxide_core::extract(&archive, &dest, &ExtractOptions::default(), &Progress::none()).unwrap();
    assert!(!dest.join("missing").exists(), "stray folders created for a missing hard-link target");
}

#[test]
fn sevenz_progress_counts_each_file_once() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    for i in 0..20 {
        fs::write(src.join(format!("f{i}.txt")), "data".repeat(100)).unwrap();
    }
    let last = Arc::new(Mutex::new(None));
    let sink = last.clone();
    let progress = Progress::with_callback(move |u| *sink.lock().unwrap() = Some((u.entries_done, u.entries_total)))
        .throttle(std::time::Duration::ZERO);
    oxide_core::create(&[src], &tmp.path().join("a.7z"), &CreateOptions::default(), &progress).unwrap();
    let (done, total) = last.lock().unwrap().unwrap();
    assert_eq!(Some(done), total, "entries_done {done} vs total {total:?}");
}

#[test]
fn case_only_name_clash_with_rename_policy_keeps_both() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("c.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    for name in ["Readme.txt", "README.txt"] {
        zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(name.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    let dest = tmp.path().join("out");
    let opts = ExtractOptions { overwrite: OverwritePolicy::Rename, ..Default::default() };
    let s = oxide_core::extract(&archive, &dest, &opts, &Progress::none()).unwrap();
    assert_eq!(s.files, 2);
    let count = fs::read_dir(&dest).unwrap().count();
    assert_eq!(count, 2, "one of the files was lost or overwritten");
}

fn raw_tar(path: &std::path::Path, entries: &[(&str, EntryType, &str, Vec<u8>)]) {
    let mut b = tar::Builder::new(fs::File::create(path).unwrap());
    for (name, kind, link, data) in entries {
        let mut h = Header::new_gnu();
        h.set_entry_type(*kind);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        if kind == &EntryType::Link {
            b.append_link(&mut h, name, link).unwrap();
        } else {
            b.append_data(&mut h, name, data.as_slice()).unwrap();
        }
    }
    b.into_inner().unwrap();
}

#[test]
fn repeated_paths_end_with_the_last_copy() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("dup.tar");
    let entries: Vec<_> =
        (0..50).map(|i| ("same.txt", EntryType::Regular, "", format!("version {i}").into_bytes())).collect();
    raw_tar(&archive, &entries);
    let dest = tmp.path().join("out");
    oxide_core::extract(&archive, &dest, &ExtractOptions::default(), &Progress::none()).unwrap();
    assert_eq!(fs::read_to_string(dest.join("same.txt")).unwrap(), "version 49");
}

#[test]
fn rename_policy_survives_clashing_generated_names() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("clash.tar");
    // Pre-existing a.txt forces "a (1).txt", which the archive also contains literally.
    let mut entries = vec![("a.txt", EntryType::Regular, "", b"new a".to_vec())];
    for i in 1..20 {
        let name: &'static str = Box::leak(format!("a ({i}).txt").into_boxed_str());
        entries.push((name, EntryType::Regular, "", name.as_bytes().to_vec()));
    }
    raw_tar(&archive, &entries);
    let dest = tmp.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("a.txt"), "old").unwrap();
    let opts = ExtractOptions { overwrite: OverwritePolicy::Rename, ..Default::default() };
    let s = oxide_core::extract(&archive, &dest, &opts, &Progress::none()).unwrap();
    assert_eq!(s.files, 20);
    assert_eq!(fs::read_dir(&dest).unwrap().count(), 21, "every file kept, none overwritten");
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
}

#[test]
fn hard_link_to_a_just_queued_file() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("hl.tar");
    let mut entries: Vec<_> = (0..30)
        .map(|i| (Box::leak(format!("f{i}.txt").into_boxed_str()) as &str, EntryType::Regular, "", vec![b'x'; 100]))
        .collect();
    entries.push(("target.txt", EntryType::Regular, "", b"payload".to_vec()));
    entries.push(("link.txt", EntryType::Link, "target.txt", Vec::new()));
    raw_tar(&archive, &entries);
    let dest = tmp.path().join("out");
    let s = oxide_core::extract(&archive, &dest, &ExtractOptions::default(), &Progress::none()).unwrap();
    assert_eq!(s.links, 1, "{:?}", s.warnings);
    assert_eq!(fs::read_to_string(dest.join("link.txt")).unwrap(), "payload");
}

#[test]
fn parallel_zip_keeps_entry_order_and_counts() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    for d in 0..10 {
        fs::create_dir_all(src.join(format!("d{d:02}"))).unwrap();
        for f in 0..30 {
            // Mixed sizes, including one over the in-memory threshold.
            let len = if d == 3 && f == 7 { 9 << 20 } else { 10 + f * 37 };
            fs::write(src.join(format!("d{d:02}/f{f:02}.txt")), vec![b'a' + (f % 26) as u8; len]).unwrap();
        }
    }
    let last = Arc::new(Mutex::new(None));
    let sink = last.clone();
    let progress = Progress::with_callback(move |u| *sink.lock().unwrap() = Some((u.entries_done, u.entries_total)))
        .throttle(std::time::Duration::ZERO);
    let archive = tmp.path().join("p.zip");
    let opts = CreateOptions { threads: 4, ..Default::default() };
    oxide_core::create(std::slice::from_ref(&src), &archive, &opts, &progress).unwrap();
    let (done, total) = last.lock().unwrap().unwrap();
    assert_eq!(Some(done), total);

    let names: Vec<String> =
        oxide_core::list(&archive, &Default::default()).unwrap().entries.into_iter().map(|e| e.path).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "entries out of order");
    assert_eq!(names.len(), 1 + 10 + 300);
    let out = tmp.path().join("out");
    oxide_core::extract(&archive, &out, &ExtractOptions::default(), &Progress::none()).unwrap();
    assert_eq!(fs::read(out.join("src/d03/f07.txt")).unwrap().len(), 9 << 20);
}
