//! Damaged-archive fuzzing: every operation on a corrupted archive must return
//! (an error is fine) without panicking, hanging, escaping the destination, or
//! leaving temporary files behind.
//!
//! A short run is part of the normal suite. For a long run:
//! `OXIDE_FUZZ_ITERS=2000 cargo test --release -p oxide-core --test fuzz`

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use common::{has_temp_files, noise};
use oxide_core::{CreateOptions, ExtractLimits, ExtractOptions, Progress, ReadOptions};
use tempfile::tempdir;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(original: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut data = original.to_vec();
    match rng.below(5) {
        // Flip a handful of bytes.
        0 | 1 => {
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(data.len());
                data[i] ^= 1 << rng.below(8);
            }
        }
        // Overwrite a run with random bytes.
        2 => {
            let start = rng.below(data.len());
            let len = 1 + rng.below(64);
            for b in data.iter_mut().skip(start).take(len) {
                *b = rng.next() as u8;
            }
        }
        // Truncate.
        3 => data.truncate(rng.below(data.len())),
        // Duplicate a slice somewhere else.
        _ => {
            let start = rng.below(data.len());
            let len = rng.below(256).min(data.len() - start);
            let chunk = data[start..start + len].to_vec();
            let at = rng.below(data.len());
            data.splice(at..at, chunk);
        }
    }
    data
}

/// Runs `f` on another thread and fails the test if it panics or takes too long.
fn guarded(what: &str, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(result.is_ok());
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(true) => {
            let _ = handle.join();
        }
        Ok(false) => panic!("{what}: panicked"),
        Err(_) => panic!("{what}: did not finish within 20 s (hang?)"),
    }
}

fn source_tree(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("nested/deeper")).unwrap();
    fs::write(src.join("a.txt"), "hello ".repeat(500)).unwrap();
    fs::write(src.join("nested/b.bin"), noise(20_000, 3)).unwrap();
    fs::write(src.join("nested/deeper/c.txt"), "c").unwrap();
    fs::write(src.join("empty"), "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
    src
}

fn fuzz_format(ext: &str, iterations: usize, seed: u64) {
    let tmp = tempdir().unwrap();
    let src = source_tree(tmp.path());
    let pristine = tmp.path().join(format!("pristine.{ext}"));
    let inputs = if matches!(ext, "gz" | "bz2" | "xz" | "zst") { vec![src.join("a.txt")] } else { vec![src] };
    let mut options = CreateOptions::default();
    if ext == "7z" || ext == "zip" {
        // Half the runs exercise the encrypted code paths too.
        if seed.is_multiple_of(2) {
            options.password = Some("pw".into());
        }
    }
    oxide_core::create(&inputs, &pristine, &options, &Progress::none()).unwrap();
    let original = fs::read(&pristine).unwrap();

    let mut rng = Rng(seed | 1);
    for i in 0..iterations {
        let data = mutate(&original, &mut rng);
        let case = tmp.path().join(format!("case{i}"));
        fs::create_dir_all(&case).unwrap();
        let archive = case.join(format!("damaged.{ext}"));
        fs::write(&archive, &data).unwrap();
        let dest = case.join("out");
        let what = format!("{ext} seed {seed} case {i}");

        let (a, d) = (archive.clone(), dest.clone());
        guarded(&what, move || {
            let read = ReadOptions { password: Some("pw".into()), format: None };
            let _ = oxide_core::list(&a, &read);
            let _ = oxide_core::test(&a, &read, &Progress::none());
            // Tight limits keep corrupted size fields from writing gigabytes.
            let limits = ExtractLimits { max_total_bytes: Some(64 << 20), ..ExtractLimits::default() };
            let opts = ExtractOptions { password: Some("pw".into()), limits, ..ExtractOptions::default() };
            let _ = oxide_core::extract(&a, &d, &opts, &Progress::none());
        });

        assert!(!has_temp_files(&case), "{what}: temporary files left behind");
        let escaped: Vec<_> = fs::read_dir(&case)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "out" && !n.starts_with("damaged."))
            .collect();
        assert!(escaped.is_empty(), "{what}: wrote outside the destination: {escaped:?}");
        let _ = fs::remove_dir_all(&case);
    }
}

fn iterations() -> usize {
    std::env::var("OXIDE_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40)
}

macro_rules! fuzz_tests {
    ($($name:ident => $ext:expr),* $(,)?) => {$(
        #[test]
        fn $name() {
            let n = iterations();
            fuzz_format($ext, n, 0x5eed_0001);
            fuzz_format($ext, n, 0x5eed_0002);
        }
    )*};
}

fuzz_tests! {
    fuzz_zip => "zip",
    fuzz_7z => "7z",
    fuzz_tar => "tar",
    fuzz_tar_gz => "tar.gz",
    fuzz_tar_bz2 => "tar.bz2",
    fuzz_tar_xz => "tar.xz",
    fuzz_tar_zst => "tar.zst",
    fuzz_gz => "gz",
    fuzz_bz2 => "bz2",
    fuzz_xz => "xz",
    fuzz_zst => "zst",
}
