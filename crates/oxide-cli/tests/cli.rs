//! End-to-end tests of the `oxide` binary.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn oxide() -> Command {
    let mut cmd = Command::cargo_bin("oxide").unwrap();
    cmd.arg("--no-progress");
    cmd
}

fn tree(root: &Path) {
    fs::create_dir_all(root.join("docs/nested")).unwrap();
    fs::write(root.join("docs/a.txt"), "alpha").unwrap();
    fs::write(root.join("docs/nested/b.txt"), "beta ✓").unwrap();
}

#[test]
fn create_list_test_extract_round_trip() {
    for ext in ["zip", "tar.zst", "tar.gz", "7z", "tar.xz"] {
        let tmp = tempdir().unwrap();
        tree(tmp.path());
        let archive = format!("backup.{ext}");
        oxide()
            .current_dir(tmp.path())
            .args(["create", "docs", "-o", &archive])
            .assert()
            .success()
            .stdout(contains("Created"));
        oxide()
            .current_dir(tmp.path())
            .args(["list", &archive])
            .assert()
            .success()
            .stdout(contains("docs/nested/b.txt"));
        oxide().current_dir(tmp.path()).args(["test", &archive]).assert().success().stdout(contains("OK"));
        oxide().current_dir(tmp.path()).args(["extract", &archive, "-o", "out"]).assert().success();
        assert_eq!(fs::read_to_string(tmp.path().join("out/docs/nested/b.txt")).unwrap(), "beta ✓", "{ext}");
    }
}

#[test]
fn json_output_is_parseable() {
    let tmp = tempdir().unwrap();
    tree(tmp.path());
    let out = oxide().current_dir(tmp.path()).args(["--json", "create", "docs", "-o", "a.zip"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["files"], 2);

    let out = oxide().current_dir(tmp.path()).args(["--json", "list", "a.zip"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["format"], "zip");
    assert_eq!(v["file_count"], 2);

    let out = oxide().args(["--json", "formats"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v.as_array().unwrap().iter().any(|f| f["name"] == "TAR.ZST" && f["can_create"] == true));
}

#[test]
fn exit_codes() {
    let tmp = tempdir().unwrap();
    tree(tmp.path());
    let dir = tmp.path();
    oxide().current_dir(dir).args(["create", "docs", "-o", "a.zip"]).assert().success();
    // Refuses to overwrite the archive, then the extracted files.
    oxide().current_dir(dir).args(["create", "docs", "-o", "a.zip"]).assert().code(6);
    oxide().current_dir(dir).args(["create", "docs", "-o", "a.zip", "--force"]).assert().success();
    oxide().current_dir(dir).args(["extract", "a.zip", "-o", "x"]).assert().success();
    oxide().current_dir(dir).args(["extract", "a.zip", "-o", "x"]).assert().code(6).stderr(contains("hint"));
    oxide()
        .current_dir(dir)
        .args(["extract", "a.zip", "-o", "x", "--overwrite", "skip"])
        .assert()
        .success()
        .stdout(contains("2 skipped"));

    // Password handling.
    oxide().current_dir(dir).args(["create", "docs", "-o", "enc.zip", "-p", "pw"]).assert().success();
    oxide().current_dir(dir).args(["extract", "enc.zip", "-o", "e1"]).assert().code(4);
    oxide().current_dir(dir).args(["extract", "enc.zip", "-o", "e2", "-p", "bad"]).assert().code(4);
    oxide()
        .current_dir(dir)
        .env("ARCHIVE_PW", "pw")
        .args(["extract", "enc.zip", "-o", "e3", "--password-env", "ARCHIVE_PW"])
        .assert()
        .success();

    // Corruption.
    let mut bytes = fs::read(dir.join("a.zip")).unwrap();
    let n = bytes.len();
    bytes.truncate(n - 30);
    fs::write(dir.join("broken.zip"), bytes).unwrap();
    oxide().current_dir(dir).args(["test", "broken.zip"]).assert().code(3);

    // Limits.
    oxide().current_dir(dir).args(["extract", "a.zip", "-o", "lim", "--max-entries", "1"]).assert().code(5);

    // Usage errors.
    oxide().args(["create", "x", "-o", "y.zip", "--level", "99"]).assert().failure();
    oxide().args(["create", "missing-input", "-o", "y.unknownext"]).assert().code(2);
    oxide().args(["bogus"]).assert().code(2);
}

#[test]
fn selective_extract_and_add() {
    let tmp = tempdir().unwrap();
    tree(tmp.path());
    let dir = tmp.path();
    oxide().current_dir(dir).args(["create", "docs", "-o", "a.tar.gz"]).assert().success();
    fs::write(dir.join("new.txt"), "fresh").unwrap();
    oxide().current_dir(dir).args(["add", "a.tar.gz", "new.txt"]).assert().success();
    oxide().current_dir(dir).args(["list", "-1", "a.tar.gz"]).assert().success().stdout(contains("new.txt"));
    oxide().current_dir(dir).args(["extract", "a.tar.gz", "-o", "sel", "--only", "docs/nested"]).assert().success();
    assert!(dir.join("sel/docs/nested/b.txt").exists());
    assert!(!dir.join("sel/docs/a.txt").exists());
    assert!(!dir.join("sel/new.txt").exists());
}

#[test]
fn quiet_mode_prints_nothing() {
    let tmp = tempdir().unwrap();
    tree(tmp.path());
    oxide().current_dir(tmp.path()).args(["-q", "create", "docs", "-o", "a.zip"]).assert().success().stdout("");
}

#[test]
fn extracts_without_wrapper_folders() {
    let tmp = tempdir().unwrap();
    tree(tmp.path());
    oxide().current_dir(tmp.path()).args(["create", "docs", "-o", "wrapped.zip"]).assert().success();
    oxide()
        .current_dir(tmp.path())
        .args(["extract", "wrapped.zip", "-o", "out", "--strip-components", "1", "--only", "./docs/nested"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(tmp.path().join("out/nested/b.txt")).unwrap(), "beta ✓");
    assert!(!tmp.path().join("out/docs").exists());
    assert!(!tmp.path().join("out/a.txt").exists());
    oxide().args(["extract", "missing.zip", "--strip-components", "-1"]).assert().code(2);
}
