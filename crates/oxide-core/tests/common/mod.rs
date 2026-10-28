#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Deterministic pseudo-random bytes (xorshift), poorly compressible.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Builds a representative tree: nested folders, Unicode names, an empty
/// file, an empty directory, compressible and incompressible data.
pub fn make_tree(root: &Path) -> PathBuf {
    let base = root.join("project");
    fs::create_dir_all(base.join("src/deep/er/est")).unwrap();
    fs::create_dir_all(base.join("empty-dir")).unwrap();
    fs::create_dir_all(base.join("ünïcødé 文件夹")).unwrap();
    fs::write(base.join("README.md"), "# Hello\n".repeat(200)).unwrap();
    fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(base.join("src/deep/er/est/leaf.txt"), "leaf").unwrap();
    fs::write(base.join("ünïcødé 文件夹/naïve café ☕.txt"), "unicode ok").unwrap();
    fs::write(base.join("empty.bin"), b"").unwrap();
    fs::write(base.join("noise.bin"), noise(300_000, 7)).unwrap();
    fs::write(base.join(".hidden"), "secret-ish").unwrap();
    base
}

/// Maps relative paths to contents (`None` for directories).
pub fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut map = BTreeMap::new();
    for entry in walk(root) {
        let rel = entry.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let meta = fs::symlink_metadata(&entry).unwrap();
        let value = if meta.is_dir() {
            None
        } else if meta.file_type().is_symlink() {
            Some(format!("-> {}", fs::read_link(&entry).unwrap().display()).into_bytes())
        } else {
            Some(fs::read(&entry).unwrap())
        };
        map.insert(rel, value);
    }
    map
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let meta = fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out
}

/// True if any temporary `.oxide-*` file was left anywhere under `root`.
pub fn has_temp_files(root: &Path) -> bool {
    walk(root).iter().any(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".oxide-")))
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}
