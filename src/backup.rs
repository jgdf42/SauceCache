use crate::{config::Config, mqtt, source};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const ENTRY: &str = "saucecache-config.json";
const MAX_JSON: u64 = 4 * 1024 * 1024;
const MAX_ZIP: u64 = 8 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Backup {
    format: String,
    version: u32,
    pub config: Config,
    mqtt_password: Option<String>,
}
// Message diagnostics must never print the exported password.
impl std::fmt::Debug for Backup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backup")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}
impl Backup {
    fn validate(&mut self) -> Result<(), String> {
        if self.format != "SauceCache configuration" || self.version != 1 {
            return Err("This is not a supported SauceCache configuration backup".into());
        }
        self.config.mqtt.validate()?;
        if !(1..=525_600).contains(&self.config.interval_minutes) {
            return Err("The backup has an invalid check interval".into());
        }
        if self.config.repositories.len() > 10_000 {
            return Err("Too many repos in backup".into());
        }
        if self
            .mqtt_password
            .as_ref()
            .is_some_and(|p| p.len() > 65_536 || self.config.mqtt.host.is_empty())
        {
            return Err("The backup has invalid MQTT credentials".into());
        }
        let mut seen = HashSet::new();
        for repo in &mut self.config.repositories {
            let normalized = source::parse(&repo.name, repo.provider)?;
            repo.name = normalized.name;
            if !seen.insert((repo.provider.to_string(), repo.name.clone())) {
                return Err("The backup contains duplicate repos".into());
            }
        }
        Ok(())
    }
    fn encode(&self) -> Result<Vec<u8>, String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if json.len() as u64 > MAX_JSON {
            return Err("Configuration is too large to back up".into());
        }
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                ENTRY,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .map_err(|e| e.to_string())?;
        writer.write_all(&json).map_err(|e| e.to_string())?;
        writer
            .finish()
            .map(|c| c.into_inner())
            .map_err(|e| e.to_string())
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_ZIP {
            return Err("Backup ZIP is too large".into());
        }
        let mut zip =
            ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Could not read the backup ZIP")?;
        if zip.len() != 1 {
            return Err("Expected a SauceCache configuration backup ZIP".into());
        }
        let entry = zip
            .by_name(ENTRY)
            .map_err(|_| "No SauceCache configuration was found in the ZIP")?;
        if entry.size() > MAX_JSON {
            return Err("Configuration in ZIP is too large".into());
        }
        let mut json = Vec::new();
        entry
            .take(MAX_JSON + 1)
            .read_to_end(&mut json)
            .map_err(|_| "The backup is corrupt or incomplete")?;
        if json.len() as u64 > MAX_JSON {
            return Err("Configuration in ZIP is too large".into());
        }
        let mut backup: Self =
            serde_json::from_slice(&json).map_err(|_| "The backup configuration is invalid")?;
        backup.validate()?;
        Ok(backup)
    }
    pub fn apply(&self) -> Result<(), String> {
        self.apply_with_save(Config::save)
    }
    fn apply_with_save(
        &self,
        save: impl FnOnce(&Config) -> Result<(), String>,
    ) -> Result<(), String> {
        mqtt::save_configuration_with(
            &self.config,
            self.mqtt_password.as_deref().unwrap_or_default(),
            save,
        )
        .map(|_| ())
    }
}

fn write_backup(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_file_name(format!(
        ".saucecache-backup-{}-{}.tmp",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|e| format!("Could not save backup: {e}"))
}

pub async fn choose_backup(config: Config) -> Result<bool, String> {
    let password = mqtt::read_password(&config.mqtt)?;
    if (config.mqtt.enabled || password.is_some()) && rfd::AsyncMessageDialog::new()
        .set_title("Keep your SauceCache backup private")
        .set_level(rfd::MessageLevel::Warning)
        .set_description("This ZIP is not encrypted and includes your saved MQTT password when one is configured. Do not share the ZIP with other people. Continue to choose where to save it?")
        .set_buttons(rfd::MessageButtons::OkCancel).show().await != rfd::MessageDialogResult::Ok {
        return Ok(false);
    }
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Back up SauceCache configuration")
        .add_filter("Configuration backup", &["zip"])
        .set_file_name(format!(
            "SauceCache-config-{}.zip",
            chrono::Local::now().format("%Y-%m-%d")
        ))
        .save_file()
        .await
    else {
        return Ok(false);
    };
    let backup = Backup {
        format: "SauceCache configuration".into(),
        version: 1,
        config,
        mqtt_password: password,
    };
    write_backup(file.path(), &backup.encode()?)?;
    Ok(true)
}

pub async fn choose_restore() -> Result<Option<Backup>, String> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Restore SauceCache configuration")
        .add_filter("Configuration backup", &["zip"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    fs::File::open(file.path())
        .map_err(|e| e.to_string())?
        .take(MAX_ZIP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let backup = Backup::decode(&bytes)?;
    if rfd::AsyncMessageDialog::new().set_title("Restore configuration?")
        .set_description("Replace your saved settings, repos, and MQTT credentials with this backup? Any incomplete edits will be replaced. Downloaded files will be kept.")
        .set_buttons(rfd::MessageButtons::OkCancel).show().await != rfd::MessageDialogResult::Ok { return Ok(None); }
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_file_can_replace_an_existing_zip_without_leaving_temporary_files() {
        let directory = std::env::temp_dir().join(format!(
            "saucecache-backup-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("backup.zip");
        fs::write(&path, b"previous backup").unwrap();
        let backup = Backup {
            format: "SauceCache configuration".into(),
            version: 1,
            config: Config::default(),
            mqtt_password: None,
        };
        write_backup(&path, &backup.encode().unwrap()).unwrap();
        assert!(Backup::decode(&fs::read(&path).unwrap()).is_ok());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }
    #[test]
    #[cfg(windows)]
    #[ignore = "Creates and removes a unique Windows test credential; run explicitly"]
    fn windows_password_restore_and_rollback() {
        let mut config = Config::default();
        config.mqtt.host = format!(
            "saucecache-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        );
        struct Cleanup(crate::config::MqttSettings);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = mqtt::save_password(&self.0, "");
            }
        }
        let _cleanup = Cleanup(config.mqtt.clone());
        mqtt::save_password(&config.mqtt, "fixture-original").unwrap();
        let backup = Backup {
            format: "SauceCache configuration".into(),
            version: 1,
            config,
            mqtt_password: Some("fixture-restored".into()),
        };
        let restored = Backup::decode(&backup.encode().unwrap()).unwrap();
        restored.apply_with_save(|_| Ok(())).unwrap();
        assert!(
            mqtt::read_password(&restored.config.mqtt)
                .unwrap()
                .as_deref()
                == Some("fixture-restored")
        );
        mqtt::save_password(&restored.config.mqtt, "fixture-before-failure").unwrap();
        assert!(
            restored
                .apply_with_save(|_| Err("Injected config write failure".into()))
                .is_err()
        );
        assert!(
            mqtt::read_password(&restored.config.mqtt)
                .unwrap()
                .as_deref()
                == Some("fixture-before-failure")
        );
        let without_password = Backup {
            mqtt_password: None,
            ..restored
        };
        without_password.apply_with_save(|_| Ok(())).unwrap();
        assert!(
            mqtt::read_password(&without_password.config.mqtt)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn zip_round_trip_preserves_config_and_password() {
        let mut config = Config {
            output_dir: "D:\\Releases".into(),
            interval_minutes: 360,
            suffixes: vec![".zip".into()],
            dark: true,
            ..Default::default()
        };
        config
            .repositories
            .push(source::parse("owner/repo", source::Provider::GitHub).unwrap());
        config.repositories[0].unavailable = true;
        config.repositories[0].suffixes_override = Some(vec![".msi".into()]);
        config.repositories[0].keep_latest = true;
        config.repositories.push(
            source::parse("https://gitlab.com/group/project", source::Provider::GitHub).unwrap(),
        );
        config.mqtt.enabled = true;
        config.mqtt.host = "localhost".into();
        let backup = Backup {
            format: "SauceCache configuration".into(),
            version: 1,
            config,
            mqtt_password: Some("fixture-secret-π".into()),
        };
        let restored = Backup::decode(&backup.encode().unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&backup.config).unwrap(),
            serde_json::to_value(&restored.config).unwrap()
        );
        assert!(restored.mqtt_password.as_deref() == Some("fixture-secret-π"));
        assert!(!format!("{restored:?}").contains("fixture-secret"));
    }
    #[test]
    fn rejects_invalid_zip_version_and_config() {
        assert!(Backup::decode(b"not a zip").is_err());
        let mut backup = Backup {
            format: "SauceCache configuration".into(),
            version: 99,
            config: Config::default(),
            mqtt_password: None,
        };
        assert!(Backup::decode(&backup.encode().unwrap()).is_err());
        backup.version = 1;
        backup.config.interval_minutes = 0;
        assert!(Backup::decode(&backup.encode().unwrap()).is_err());
        backup.config.interval_minutes = 30;
        let restored = Backup::decode(&backup.encode().unwrap()).unwrap();
        assert!(restored.mqtt_password.is_none());
        assert!(restored.config.repositories.is_empty());
    }
    #[test]
    fn rejects_unrelated_and_oversized_archives_without_extracting() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../config.json", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"{}").unwrap();
        assert!(Backup::decode(&writer.finish().unwrap().into_inner()).is_err());
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                ENTRY,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer
            .write_all(&vec![b' '; MAX_JSON as usize + 1])
            .unwrap();
        assert!(Backup::decode(&writer.finish().unwrap().into_inner()).is_err());
    }
}
