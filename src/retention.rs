//! Only a successful release can retire previously recorded downloads.
//! Never recursively delete a release directory or infer ownership from its name.
use crate::github::safe_filename;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct DownloadedFile {
    pub name: String,
    pub size: u64,
}
#[derive(Serialize, Deserialize)]
struct CompletedRelease {
    folder: String,
    files: Vec<DownloadedFile>,
}
#[derive(Serialize, Deserialize)]
struct History {
    repository: String,
    releases: Vec<CompletedRelease>,
}

fn plain(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if metadata.is_symlink() || reparse {
        return Err(format!("Cleanup refused a linked path: {}", path.display()));
    }
    Ok(metadata)
}

fn validate(history: &History) -> Result<(), String> {
    for release in &history.releases {
        if !safe_filename(&release.folder) || release.folder.eq_ignore_ascii_case(".saucecache") {
            return Err("Invalid release folder in download history".into());
        }
        if release.files.iter().any(|file| !safe_filename(&file.name)) {
            return Err("Invalid filename in download history".into());
        }
    }
    Ok(())
}

fn save(directory: &Path, history: &History) -> Result<(), String> {
    let path = directory.join("releases.json");
    let temp = directory.join("releases.tmp");
    for candidate in [&path, &temp] {
        if candidate.try_exists().map_err(|e| e.to_string())? {
            plain(candidate)?;
        }
    }
    let mut output = fs::File::create(&temp).map_err(|e| e.to_string())?;
    output
        .write_all(&serde_json::to_vec(history).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    fs::rename(temp, path).map_err(|e| e.to_string())
}

/// Called only when ALL selected files of this release are verified complete.
pub fn complete(
    root: &Path,
    repository: &str,
    folder: &str,
    files: Vec<DownloadedFile>,
    keep_latest: bool,
    cancel: &AtomicBool,
) -> Result<usize, String> {
    if files.is_empty() || cancel.load(Ordering::Relaxed) {
        return Ok(0);
    }
    let root = std::path::absolute(root).map_err(|e| e.to_string())?;
    if !safe_filename(folder)
        || folder.eq_ignore_ascii_case(".saucecache")
        || files.iter().any(|file| !safe_filename(&file.name))
    {
        return Err("Invalid completed release path".into());
    }
    for ancestor in root.ancestors() {
        plain(ancestor)?;
    }
    plain(&root.join(folder))?;
    for file in &files {
        let metadata = plain(&root.join(folder).join(&file.name))?;
        if !metadata.is_file() || metadata.len() != file.size {
            return Err("Replacement release is incomplete; older versions were kept".into());
        }
    }
    let directory = root.join(".saucecache");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    plain(&directory)?;
    let path = directory.join("releases.json");
    let mut history = if path.try_exists().map_err(|e| e.to_string())? {
        plain(&path)?;
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .map_err(|e| e.to_string())?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("Download history is too large; older versions were kept".into());
        }
        serde_json::from_slice::<History>(&bytes).map_err(|e| e.to_string())?
    } else {
        History {
            repository: repository.to_owned(),
            releases: Vec::new(),
        }
    };
    if !history.repository.eq_ignore_ascii_case(repository) {
        return Err(
            "This download folder belongs to another repo; older versions were kept".into(),
        );
    }
    validate(&history)?;
    let mut current = CompletedRelease {
        folder: folder.into(),
        files,
    };
    if let Some(index) = history
        .releases
        .iter()
        .position(|release| release.folder.eq_ignore_ascii_case(folder))
    {
        for file in history.releases.remove(index).files {
            if !current
                .files
                .iter()
                .any(|f| f.name.eq_ignore_ascii_case(&file.name))
            {
                current.files.push(file);
            }
        }
    }
    history.releases.push(current);
    validate(&history)?;
    // Record the completed replacement BEFORE touching any previous release.
    save(&directory, &history)?;
    if !keep_latest || cancel.load(Ordering::Relaxed) {
        return Ok(0);
    }
    let mut removed = 0;
    for release in &history.releases[..history.releases.len() - 1] {
        let old = root.join(&release.folder);
        if !old.try_exists().map_err(|e| e.to_string())? {
            continue;
        }
        if !plain(&old)?.is_dir() {
            return Err("Previous release path is not a directory".into());
        }
        let receipts = old.join(".saucecache");
        if receipts.try_exists().map_err(|e| e.to_string())? {
            plain(&receipts)?;
        }
        for file in &release.files {
            if cancel.load(Ordering::Relaxed) {
                return Ok(removed);
            }
            let target = old.join(&file.name);
            if target.try_exists().map_err(|e| e.to_string())? {
                let metadata = plain(&target)?;
                if !metadata.is_file() || metadata.len() != file.size {
                    return Err(format!("Older file changed; kept {}", target.display()));
                }
                fs::remove_file(&target).map_err(|e| e.to_string())?;
                removed += 1;
            }
            let receipt = receipts.join(format!("{}.json", file.name));
            if receipt.try_exists().map_err(|e| e.to_string())? {
                plain(&receipt)?;
                fs::remove_file(receipt).map_err(|e| e.to_string())?;
            }
        }
        // These succeed only when empty; manually added files/folders stay put.
        let _ = fs::remove_dir(&receipts);
        let _ = fs::remove_dir(&old);
    }
    let latest = history.releases.pop().unwrap();
    history.releases = vec![latest];
    save(&directory, &history)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "saucecache-retention-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }
        fn release(&self, version: &str) -> Vec<DownloadedFile> {
            fs::create_dir_all(self.0.join(version)).unwrap();
            fs::write(self.0.join(version).join("app.zip"), b"zip").unwrap();
            vec![DownloadedFile {
                name: "app.zip".into(),
                size: 3,
            }]
        }
        fn complete(&self, version: &str, keep: bool) -> Result<usize, String> {
            complete(
                &self.0,
                "owner/repo",
                version,
                self.release(version),
                keep,
                &AtomicBool::new(false),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn default_keeps_versions_and_opt_in_removes_only_recorded_files() {
        let fixture = Fixture::new();
        fixture.complete("v1", false).unwrap();
        fixture.complete("v2", false).unwrap();
        fs::write(fixture.0.join("v1/notes.txt"), b"mine").unwrap();
        fixture.release("untracked");
        assert_eq!(fixture.complete("v3", true).unwrap(), 2);
        assert!(fixture.0.join("v1/notes.txt").exists());
        assert!(!fixture.0.join("v2").exists());
        assert!(fixture.0.join("v3/app.zip").exists());
        assert!(fixture.0.join("untracked/app.zip").exists());
    }
    #[test]
    fn cancellation_and_no_matches_never_remove_previous_version() {
        let fixture = Fixture::new();
        fixture.complete("v1", false).unwrap();
        complete(
            &fixture.0,
            "owner/repo",
            "v2",
            vec![],
            true,
            &AtomicBool::new(false),
        )
        .unwrap();
        complete(
            &fixture.0,
            "owner/repo",
            "v2",
            fixture.release("v2"),
            true,
            &AtomicBool::new(true),
        )
        .unwrap();
        assert!(fixture.0.join("v1/app.zip").exists());
    }
    #[test]
    fn malformed_history_and_another_repo_cannot_trigger_deletion() {
        let fixture = Fixture::new();
        fixture.complete("v1", false).unwrap();
        assert!(
            complete(
                &fixture.0,
                "other/repo",
                "v2",
                fixture.release("v2"),
                true,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        fs::write(
            fixture.0.join(".saucecache/releases.json"),
            r#"{"repository":"owner/repo","releases":[{"folder":"..","files":[]}]}"#,
        )
        .unwrap();
        assert!(fixture.complete("v3", true).is_err());
        assert!(fixture.0.join("v1/app.zip").exists());
    }
    #[test]
    fn changed_old_files_are_kept_and_cleanup_can_retry_after_restart() {
        let fixture = Fixture::new();
        fixture.complete("v1", false).unwrap();
        fs::write(fixture.0.join("v1/app.zip"), b"user edit").unwrap();
        assert!(fixture.complete("v2", true).is_err());
        assert!(fixture.0.join("v1/app.zip").exists());
        fs::write(fixture.0.join("v1/app.zip"), b"zip").unwrap();
        assert_eq!(fixture.complete("v2", true).unwrap(), 1);
        assert!(!fixture.0.join("v1").exists());
    }
    #[cfg(windows)]
    #[test]
    fn cleanup_refuses_junctions_pointing_outside_the_repo() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fixture.complete("v1", false).unwrap();
        outside.release("v1");
        let junction = fixture.0.join("v1");
        fs::remove_file(junction.join("app.zip")).unwrap();
        fs::remove_dir(&junction).unwrap();
        let command = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(outside.0.join("v1"))
            .output()
            .unwrap();
        assert!(
            command.status.success(),
            "Could not create the isolated test junction"
        );
        let result = fixture.complete("v2", true);
        // Remove the junction itself; never traverse it during fixture cleanup.
        fs::remove_dir(&junction).unwrap();
        assert!(result.is_err());
        assert!(outside.0.join("v1/app.zip").exists());
    }
}
