//! "Extract here", as used when an archive is opened from Finder: unpack next to
//! the archive the way macOS's Archive Utility does.
//!
//! Everything is extracted into a hidden staging folder beside the archive first,
//! so nothing appears half-done. Then a single top-level item is moved out to sit
//! next to the archive; several items are kept together in a folder named after
//! the archive. Existing names are never replaced: "name 2", "name 3", ... are used.

use std::fs;
use std::path::{Path, PathBuf};

use oxide_core::{ArchiveFormat, ExtractOptions, OperationSummary, OverwritePolicy, OxideError, Password, Progress};

/// Extracts `archive` beside itself and returns where the result ended up.
pub fn extract_beside(
    archive: &Path,
    password: Option<Password>,
    progress: &Progress,
) -> Result<(OperationSummary, PathBuf), OxideError> {
    let parent = match archive.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let staging = tempfile::Builder::new()
        .prefix(".oxide-extract-")
        .tempdir_in(&parent)
        .map_err(|e| OxideError::io(&parent, e))?;
    // The staging folder starts empty, so a clash can only come from inside the
    // archive (e.g. `Readme.txt` and `README.txt` on a case-insensitive disk): keep both.
    let options = ExtractOptions { password, overwrite: OverwritePolicy::Rename, ..ExtractOptions::default() };
    let summary = oxide_core::extract(archive, staging.path(), &options, progress)?;

    let items: Vec<fs::DirEntry> =
        fs::read_dir(staging.path()).and_then(|it| it.collect()).map_err(|e| OxideError::io(staging.path(), e))?;
    let target = if let [only] = items.as_slice() {
        let is_dir = only.file_type().is_ok_and(|t| t.is_dir());
        let dest = unique_path(&parent, &only.file_name().to_string_lossy(), is_dir);
        fs::rename(only.path(), &dest).map_err(|e| OxideError::io(&dest, e))?;
        dest // the now-empty staging folder is removed when `staging` drops
    } else {
        let dest = unique_path(&parent, &archive_stem(archive), true);
        let kept = staging.keep();
        // Temporary folders are private (0700); give the result normal permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&kept, fs::Permissions::from_mode(0o755));
        }
        if let Err(e) = fs::rename(&kept, &dest) {
            let _ = fs::remove_dir_all(&kept);
            return Err(OxideError::io(&dest, e));
        }
        dest
    };
    Ok((summary, target))
}

/// "Photos.tar.gz" → "Photos".
fn archive_stem(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Archive".into());
    let lower = name.to_ascii_lowercase();
    let longest = ArchiveFormat::ALL
        .iter()
        .flat_map(|f| f.extensions().iter())
        .filter(|ext| lower.ends_with(&format!(".{ext}")) && name.len() > ext.len() + 1)
        .map(|ext| ext.len() + 1)
        .max();
    match longest {
        Some(len) => name[..name.len() - len].to_owned(),
        None => Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name),
    }
}

/// `parent/name`, or the first free "name 2", "name 3", ... (before any file extension).
fn unique_path(parent: &Path, name: &str, is_dir: bool) -> PathBuf {
    let candidate = parent.join(name);
    if fs::symlink_metadata(&candidate).is_err() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !is_dir && !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| parent.join(format!("{stem} {n}{ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .unwrap_or(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip(path: &Path, files: &[&str]) {
        let dir = tempfile::tempdir().unwrap();
        let mut inputs = Vec::new();
        for f in files {
            let p = dir.path().join(f);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, f.as_bytes()).unwrap();
            let top = dir.path().join(f.split('/').next().unwrap());
            if !inputs.contains(&top) {
                inputs.push(top);
            }
        }
        oxide_core::create(&inputs, path, &Default::default(), &Progress::none()).unwrap();
    }

    #[test]
    fn single_folder_lands_beside_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("Project.zip");
        zip(&archive, &["Project/a.txt", "Project/b.txt"]);
        let (_, out) = extract_beside(&archive, None, &Progress::none()).unwrap();
        assert_eq!(out, tmp.path().join("Project"));
        assert!(out.join("a.txt").is_file());
        // Again: never overwrite, use "Project 2".
        let (_, again) = extract_beside(&archive, None, &Progress::none()).unwrap();
        assert_eq!(again, tmp.path().join("Project 2"));
        let leftovers = fs::read_dir(tmp.path())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".oxide-"));
        assert_eq!(leftovers.count(), 0, "staging folder left behind");
    }

    #[test]
    fn single_file_keeps_extension_when_renamed() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("report.zip");
        zip(&archive, &["report.txt"]);
        fs::write(tmp.path().join("report.txt"), "existing").unwrap();
        let (_, out) = extract_beside(&archive, None, &Progress::none()).unwrap();
        assert_eq!(out, tmp.path().join("report 2.txt"));
        assert_eq!(fs::read_to_string(tmp.path().join("report.txt")).unwrap(), "existing");
    }

    #[test]
    fn several_items_go_into_a_folder_named_after_the_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("Photos.tar.gz");
        zip(&archive, &["one.txt", "two.txt"]);
        let (summary, out) = extract_beside(&archive, None, &Progress::none()).unwrap();
        assert_eq!(out, tmp.path().join("Photos"));
        assert_eq!(summary.files, 2);
        assert!(out.join("one.txt").is_file() && out.join("two.txt").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn failures_leave_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("broken.zip");
        fs::write(&archive, b"PK\x03\x04 definitely not a real zip").unwrap();
        assert!(extract_beside(&archive, None, &Progress::none()).is_err());
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("broken.zip")]);
    }
}
