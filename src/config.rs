use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Repository {
    pub name: String,
    pub unavailable: bool,
    pub provider: crate::source::Provider,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MqttSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub tls: bool,
    pub topic: String,
    pub publish_status: bool,
    pub publish_repositories: bool,
    pub publish_downloads: bool,
}
impl Default for MqttSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 1883,
            username: String::new(),
            tls: false,
            topic: "saucecache-rust".into(),
            publish_status: true,
            publish_repositories: true,
            publish_downloads: true,
        }
    }
}
impl MqttSettings {
    pub fn validated_with_port(&self, port: &str) -> Result<Self, String> {
        let mut settings = self.clone();
        settings.host = settings.host.trim().into();
        settings.topic = settings.topic.trim().trim_matches('/').into();
        settings.port = port
            .trim()
            .parse()
            .map_err(|_| "MQTT port must be 1–65535")?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.host.trim().is_empty() {
            return Err("Enter a broker hostname or IP".into());
        }
        if self.port == 0 {
            return Err("MQTT port must be 1–65535".into());
        }
        if self.topic.trim_matches('/').is_empty() || self.topic.contains(['+', '#', '\0']) {
            return Err("Enter a topic prefix without + or #".into());
        }
        Ok(())
    }
    pub fn credential(&self) -> String {
        format!("{}:{}/{}", self.host, self.port, self.username)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub repositories: Vec<Repository>,
    pub output_dir: String,
    pub interval_minutes: u64,
    pub suffixes: Vec<String>,
    pub dark: bool,
    pub close_to_tray: bool,
    pub mqtt: MqttSettings,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            repositories: vec![],
            output_dir: String::new(),
            interval_minutes: 30,
            suffixes: vec![],
            dark: false,
            close_to_tray: true,
            mqtt: MqttSettings::default(),
        }
    }
}
pub fn directory() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_else(|| ".".into())).join("SauceCacheRust")
}
impl Config {
    pub fn load() -> Result<Self, String> {
        let file = directory().join("config.json");
        if !file.exists() {
            return Ok(Self::default());
        }
        serde_json::from_slice(&fs::read(file).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
    pub fn save(&self) -> Result<(), String> {
        fs::create_dir_all(directory()).map_err(|e| e.to_string())?;
        let file = directory().join("config.json");
        let temp = directory().join("config.tmp");
        fs::write(
            &temp,
            serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(temp, file).map_err(|e| e.to_string())
    }
}
pub fn parse_repository(input: &str) -> Result<String, String> {
    let mut value = input.trim().to_owned();
    if value.to_lowercase().starts_with("github.com/")
        || value.to_lowercase().starts_with("www.github.com/")
    {
        value = format!("https://{value}");
    }
    let path = if value.contains("://") {
        let url = url::Url::parse(&value).map_err(|_| "Invalid GitHub URL")?;
        if !["http", "https"].contains(&url.scheme())
            || ![Some("github.com"), Some("www.github.com")].contains(&url.host_str())
        {
            return Err("Use a github.com URL".into());
        }
        url.path().to_owned()
    } else {
        value
    };
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if parts.len() < 2 {
        return Err("Enter owner/repository or a full GitHub URL".into());
    }
    let owner = parts[0];
    let repo = parts[1].trim_end_matches(".git");
    if [owner, repo].iter().any(|part| {
        part.is_empty()
            || *part == "."
            || *part == ".."
            || !part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    }) {
        return Err("Invalid repository name".into());
    }
    Ok(format!("{owner}/{repo}"))
}
pub fn suffixes(input: &str) -> Vec<String> {
    let mut result = vec![];
    for item in input.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        if !result.iter().any(|v| v == item) {
            result.push(item.into());
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mqtt_editor_rejects_partial_values_without_changing_saved_settings() {
        let original = MqttSettings::default();
        for port in ["", "0", "12x", "65536"] {
            assert!(original.validated_with_port(port).is_err());
        }
        assert_eq!(original.port, 1883);
        let mut draft = original.clone();
        draft.enabled = true;
        assert!(draft.validated_with_port("1883").is_err());
        draft.host = " localhost ".into();
        draft.topic = " /saucecache/ ".into();
        let saved = draft.validated_with_port("8883").unwrap();
        assert_eq!(saved.host, "localhost");
        assert_eq!(saved.topic, "saucecache");
        assert_eq!(saved.port, 8883);
    }
    #[test]
    fn clean_defaults() {
        let c = Config::default();
        assert!(c.repositories.is_empty());
        assert!(c.output_dir.is_empty());
        assert!(c.close_to_tray);
        assert!(!c.mqtt.enabled);
    }
    #[test]
    fn urls() {
        assert_eq!(
            parse_repository("github.com/owner/repo/releases/tag/v1").unwrap(),
            "owner/repo"
        );
        assert!(parse_repository("https://evil.com/owner/repo").is_err());
        assert!(parse_repository("a/..").is_err());
    }
    #[test]
    fn filters() {
        assert_eq!(suffixes(" .zip, .msi,, .zip"), vec![".zip", ".msi"]);
    }
}
