use crate::config::{Config, Repository};
use crate::source::{self, Provider};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub enum Event {
    SourceResolved {
        previous: String,
        repository: Repository,
    },
    Log(String),
    Availability(String, bool),
    Progress(String, u64, u64),
    Download {
        repository: String,
        tag: String,
        asset: String,
        size: u64,
    },
    Finished,
    Backoff(u64),
    Mqtt(bool, String),
    MqttTest(String),
    Tray(TrayCommand),
}
#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    Show,
    Check,
    Start,
    Stop,
    Exit,
}
#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    browser_download_url: String,
}
#[derive(Clone, Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
    #[serde(default)]
    draft: bool,
}
pub struct Watcher {
    client: Client,
    api_base: String,
    cache: HashMap<String, (String, Vec<Release>)>,
    resolved: HashMap<String, Repository>,
    detected_hosts: HashMap<String, Provider>,
    #[cfg(test)]
    source_test_origin: Option<String>,
}
#[derive(Debug)]
enum FetchError {
    ProviderMismatch(String),
    Unavailable(String),
    Transient(String),
    RateLimited(u64),
}
impl Watcher {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("SauceCache/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(90))
                .build()
                .map_err(|e| e.to_string())?,
            cache: HashMap::new(),
            resolved: HashMap::new(),
            detected_hosts: HashMap::new(),
            #[cfg(test)]
            source_test_origin: None,
            api_base: "https://api.github.com".into(),
        })
    }
    fn request(&self, url: &str) -> reqwest::blocking::RequestBuilder {
        let mut request = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if url::Url::parse(url)
            .ok()
            .is_some_and(|url| url.host_str() == Some("api.github.com"))
            && let Ok(token) = std::env::var("GITHUB_TOKEN")
        {
            request = request.bearer_auth(token);
        }
        request
    }
    fn backoff(response: &Response) -> Option<u64> {
        let status = response.status().as_u16();
        if status != 429 && status != 403 {
            return None;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(seconds) = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            return Some(seconds.max(60));
        }
        if response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|h| h.to_str().ok())
            == Some("0")
        {
            return Some(
                response
                    .headers()
                    .get("x-ratelimit-reset")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(now + 60)
                    .saturating_sub(now)
                    .max(60)
                    + 1,
            );
        }
        // Secondary limits can omit explicit reset headers. A short global
        // pause also avoids hammering the API on permission-related 403s.
        Some(60)
    }
    fn latest(&mut self, repo: &str) -> Result<Option<Release>, FetchError> {
        let url = format!("{}/repos/{repo}/releases?per_page=10", self.api_base);
        let mut request = self.request(&url);
        if let Some((etag, _)) = self.cache.get(repo) {
            request = request.header("If-None-Match", etag);
        }
        let response = request
            .send()
            .map_err(|_| FetchError::Transient("Could not reach GitHub".into()))?;
        if let Some(seconds) = Self::backoff(&response) {
            return Err(FetchError::RateLimited(seconds));
        }
        if response.status().as_u16() == 304 {
            return Ok(self
                .cache
                .get(repo)
                .and_then(|(_, r)| r.iter().find(|r| !r.draft).cloned()));
        }
        if [404, 410, 451].contains(&response.status().as_u16()) {
            let verify = self
                .request(&format!("{}/repos/{repo}", self.api_base))
                .send()
                .map_err(|_| {
                    FetchError::Transient("Could not verify repository availability".into())
                })?;
            if let Some(seconds) = Self::backoff(&verify) {
                return Err(FetchError::RateLimited(seconds));
            }
            if [404, 410, 451].contains(&verify.status().as_u16()) {
                return Err(FetchError::Unavailable(format!(
                    "HTTP {}: missing, restricted, or inaccessible",
                    verify.status().as_u16()
                )));
            }
        }
        if !response.status().is_success() {
            return Err(FetchError::Transient(format!(
                "GitHub returned HTTP {}",
                response.status()
            )));
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let releases: Vec<Release> = response
            .json()
            .map_err(|_| FetchError::Transient("Invalid release response".into()))?;
        let release = releases.iter().find(|r| !r.draft).cloned();
        if let Some(etag) = etag {
            self.cache.insert(repo.into(), (etag, releases));
        }
        Ok(release)
    }
    fn latest_source(&mut self, repo: &Repository) -> Result<Option<Release>, FetchError> {
        let mut resolved = self
            .resolved
            .get(&repo.name)
            .cloned()
            .unwrap_or_else(|| repo.clone());
        if resolved.provider == Provider::Auto {
            resolved = self.detect_source(&resolved)?;
            self.resolved.insert(repo.name.clone(), resolved.clone());
        }
        match self.latest_resolved(&resolved) {
            Err(FetchError::ProviderMismatch(reason)) => match self.detect_source(repo) {
                Ok(detected) if detected.provider != resolved.provider => {
                    self.resolved.insert(repo.name.clone(), detected.clone());
                    self.latest_resolved(&detected)
                }
                Err(FetchError::RateLimited(seconds)) => Err(FetchError::RateLimited(seconds)),
                _ => Err(FetchError::Transient(format!(
                    "{reason}; the host rejected its release API request"
                ))),
            },
            result => result,
        }
    }
    fn source_request(&self, url: &str) -> reqwest::blocking::RequestBuilder {
        #[cfg(test)]
        let url = if let Some(origin) = &self.source_test_origin {
            let parsed = url::Url::parse(url).unwrap();
            format!(
                "{origin}{}{}",
                parsed.path(),
                parsed.query().map(|q| format!("?{q}")).unwrap_or_default()
            )
        } else {
            url.to_owned()
        };
        self.client.get(url).header("Accept", "application/json")
    }
    fn inspect_api(&self, url: &str) -> Result<(u16, Option<serde_json::Value>), FetchError> {
        let response = self
            .source_request(url)
            .timeout(Duration::from_secs(12))
            .send()
            .map_err(|_| {
                FetchError::Transient("Could not inspect the release server API".into())
            })?;
        let status = response.status().as_u16();
        if (status == 429
            || response.headers().contains_key("retry-after")
            || response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|value| value == "0"))
            && let Some(seconds) = Self::backoff(&response)
        {
            return Err(FetchError::RateLimited(seconds));
        }
        let mut body = Vec::new();
        response
            .take(1_048_577)
            .read_to_end(&mut body)
            .map_err(|_| FetchError::Transient("Could not inspect API metadata".into()))?;
        let json = if body.len() <= 1_048_576 {
            serde_json::from_slice(&body).ok()
        } else {
            None
        };
        Ok((status, json))
    }
    fn detect_source(&mut self, repo: &Repository) -> Result<Repository, FetchError> {
        let url = url::Url::parse(&repo.name).map_err(|_| {
            FetchError::Transient("Enter a full HTTPS repo URL for automatic detection".into())
        })?;
        let origin = url.origin().ascii_serialization();
        if let Some(provider) = self.detected_hosts.get(&origin) {
            return source::parse(&repo.name, *provider).map_err(FetchError::Transient);
        }
        let mut candidates = Vec::new();
        for provider in [Provider::Gitea, Provider::GitLab] {
            if let Ok(candidate) = source::parse(&repo.name, provider) {
                candidates.push(candidate);
            }
        }
        for candidate in &candidates {
            let spec = source::spec(candidate).map_err(FetchError::Transient)?;
            match self.inspect_api(&spec.verify) {
                Ok((200, Some(payload))) if identifies_project(candidate, &payload) => {
                    self.detected_hosts
                        .insert(origin.clone(), candidate.provider);
                    return Ok(candidate.clone());
                }
                Err(FetchError::RateLimited(seconds)) => {
                    return Err(FetchError::RateLimited(seconds));
                }
                _ => {}
            }
        }
        // Version endpoints can identify a server even when this particular repo is gone.
        for candidate in candidates {
            let version = match candidate.provider {
                Provider::Gitea => "/api/v1/version",
                Provider::GitLab => "/api/v4/version",
                _ => continue,
            };
            match self.inspect_api(&format!("{origin}{version}")) {
                Ok((200, Some(payload)))
                    if payload["version"].as_str().is_some_and(|v| {
                        !v.is_empty() && v.len() < 96 && v.as_bytes()[0].is_ascii_digit()
                    }) =>
                {
                    self.detected_hosts
                        .insert(origin.clone(), candidate.provider);
                    return Ok(candidate);
                }
                Err(FetchError::RateLimited(seconds)) => {
                    return Err(FetchError::RateLimited(seconds));
                }
                _ => {}
            }
        }
        Err(FetchError::Transient("No supported public Gitea/Forgejo or GitLab API was detected. The host may be offline, private, or unsupported.".into()))
    }
    fn latest_resolved(&mut self, repo: &Repository) -> Result<Option<Release>, FetchError> {
        if repo.provider == Provider::GitHub {
            return self.latest(&repo.name);
        }
        let spec = source::spec(repo).map_err(FetchError::Transient)?;
        let mut request = self.source_request(&spec.releases);
        if let Some((etag, _)) = self.cache.get(&repo.name) {
            request = request.header("If-None-Match", etag);
        }
        let response = request
            .send()
            .map_err(|_| FetchError::Transient("Could not reach release host".into()))?;
        if response.status().as_u16() == 405 {
            return Err(FetchError::ProviderMismatch(
                "Release host returned HTTP 405 Method Not Allowed".into(),
            ));
        }
        if let Some(seconds) = Self::backoff(&response) {
            return Err(FetchError::RateLimited(seconds));
        }
        if response.status().as_u16() == 304 {
            return Ok(self
                .cache
                .get(&repo.name)
                .and_then(|(_, r)| r.first().cloned()));
        }
        if [404, 410, 451].contains(&response.status().as_u16()) {
            let verify = self
                .source_request(&spec.verify)
                .send()
                .map_err(|_| FetchError::Transient("Could not verify repo status".into()))?;
            if let Some(seconds) = Self::backoff(&verify) {
                return Err(FetchError::RateLimited(seconds));
            }
            if [404, 410, 451].contains(&verify.status().as_u16()) {
                return Err(FetchError::Unavailable(format!(
                    "HTTP {}: missing or inaccessible",
                    verify.status().as_u16()
                )));
            }
        }
        if !response.status().is_success() {
            return Err(FetchError::Transient(format!(
                "Release host returned HTTP {}",
                response.status()
            )));
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let payload: serde_json::Value = response
            .json()
            .map_err(|_| FetchError::Transient("Unsupported release metadata".into()))?;
        let release = decode_release(repo.provider, payload).map_err(FetchError::Transient)?;
        if let Some(etag) = etag {
            self.cache.insert(
                repo.name.clone(),
                (etag, release.clone().into_iter().collect()),
            );
        }
        Ok(release)
    }
    pub fn check(&mut self, config: Config, cancel: Arc<AtomicBool>, send: Sender<Event>) {
        for mut repo in config.repositories {
            let unavailable = repo.unavailable;
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let _ = send.send(Event::Log(format!("Checking {}…", repo.name)));
            let result = self.latest_source(&repo);
            if let Some(mut detected) = self.resolved.get(&repo.name).cloned() {
                detected.unavailable = repo.unavailable;
                detected.suffixes_override = repo.suffixes_override.clone();
                detected.keep_latest = repo.keep_latest;
                if detected.provider != repo.provider || detected.name != repo.name {
                    let _ = send.send(Event::SourceResolved {
                        previous: repo.name.clone(),
                        repository: detected.clone(),
                    });
                    let _ = send.send(Event::Log(format!(
                        "Detected {} server for {}",
                        detected.provider, detected.name
                    )));
                    repo = detected;
                }
            }
            let name = repo.name.clone();
            let release = match result {
                Ok(release) => {
                    if unavailable {
                        let _ = send.send(Event::Availability(name.clone(), false));
                    }
                    release
                }
                Err(FetchError::Unavailable(reason)) => {
                    let _ = send.send(Event::Availability(name.clone(), true));
                    let _ = send.send(Event::Log(format!("Unavailable: {name}: {reason}")));
                    continue;
                }
                Err(FetchError::Transient(reason) | FetchError::ProviderMismatch(reason)) => {
                    let _ = send.send(Event::Log(format!("{name}: {reason}")));
                    continue;
                }
                Err(FetchError::RateLimited(seconds)) => {
                    let _ = send.send(Event::Log(format!(
                        "Host rate limit: pausing checks for {seconds} seconds"
                    )));
                    let _ = send.send(Event::Backoff(seconds));
                    break;
                }
            };
            let Some(release) = release else {
                let _ = send.send(Event::Log(format!("{name}: no published releases")));
                continue;
            };
            let storage = match source::spec(&repo) {
                Ok(spec) => spec.storage,
                Err(error) => {
                    let _ = send.send(Event::Log(error));
                    continue;
                }
            };
            let root = PathBuf::from(&config.output_dir).join(storage);
            let version = tag_folder(&release.tag_name);
            let folder = root.join(&version);
            let suffixes = repo.suffixes_override.as_ref().unwrap_or(&config.suffixes);
            let assets: Vec<_> = release
                .assets
                .iter()
                .filter(|asset| {
                    suffixes.is_empty()
                        || suffixes.iter().any(|suffix| {
                            asset.name.to_lowercase().ends_with(&suffix.to_lowercase())
                        })
                })
                .collect();
            if assets.is_empty() {
                let _ = send.send(Event::Log(format!(
                    "{name}: no assets match the extension filter; previous versions kept"
                )));
                continue;
            }
            let mut completed = true;
            let mut files = Vec::new();
            for asset in assets {
                if cancel.load(Ordering::Relaxed) {
                    completed = false;
                    break;
                }
                if !safe_filename(&asset.name) {
                    completed = false;
                    let _ = send.send(Event::Log(format!(
                        "Skipped unsafe filename: {}",
                        asset.name
                    )));
                    continue;
                }
                let target = folder.join(&asset.name);
                if already_saved(asset, &target) {
                    let _ = send.send(Event::Log(format!("Already saved: {}", asset.name)));
                    if let Ok(metadata) = target.metadata() {
                        files.push(crate::retention::DownloadedFile {
                            name: asset.name.clone(),
                            size: metadata.len(),
                        });
                    } else {
                        completed = false;
                    }
                    continue;
                }
                match self.download(asset, &target, &cancel, &send) {
                    Ok(size) => {
                        files.push(crate::retention::DownloadedFile {
                            name: asset.name.clone(),
                            size,
                        });
                        let _ = send.send(Event::Log(format!("Saved: {}", target.display())));
                        let _ = send.send(Event::Download {
                            repository: name.clone(),
                            tag: release.tag_name.clone(),
                            asset: asset.name.clone(),
                            size,
                        });
                    }
                    Err(error) => {
                        completed = false;
                        let _ = send.send(Event::Log(error));
                    }
                }
            }
            if completed && !cancel.load(Ordering::Relaxed) {
                match crate::retention::complete(
                    &root,
                    &name,
                    &version,
                    files,
                    repo.keep_latest,
                    &cancel,
                ) {
                    Ok(removed) if removed > 0 => {
                        let _ = send.send(Event::Log(format!("{name}: removed {removed} older downloaded file(s) after completing {}", release.tag_name)));
                    }
                    Err(error) => {
                        let _ =
                            send.send(Event::Log(format!("{name}: older versions kept: {error}")));
                    }
                    _ => {}
                }
            }
        }
        let _ = send.send(Event::Finished);
    }
    fn download(
        &self,
        asset: &Asset,
        target: &Path,
        cancel: &AtomicBool,
        send: &Sender<Event>,
    ) -> Result<u64, String> {
        fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        let part = target.with_file_name(format!("{}.part", asset.name));
        let result = (|| {
            // Asset URLs come from the selected provider's release API. No API
            // credentials are attached to cross-host release downloads.
            let url = url::Url::parse(&asset.browser_download_url).map_err(|e| e.to_string())?;
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
                return Err("Unsupported asset URL".into());
            }
            #[cfg(test)]
            let url = if let Some(origin) = &self.source_test_origin {
                url::Url::parse(&format!("{origin}{}", url.path())).unwrap()
            } else {
                url
            };
            let mut response = self
                .client
                .get(url)
                .send()
                .and_then(Response::error_for_status)
                .map_err(|e| {
                    format!(
                        "Download failed: {} ({})",
                        asset.name,
                        e.status().map_or("network error".into(), |s| s.to_string())
                    )
                })?;
            if response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| s.starts_with("text/html"))
            {
                return Err(format!("Skipped webpage link: {}", asset.name));
            }
            let expected = asset.size.or(response.content_length());
            let mut output = File::create(&part).map_err(|e| e.to_string())?;
            let mut buffer = [0; 128 * 1024];
            let mut done = 0;
            let mut last = std::time::Instant::now();
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("Download stopped".into());
                }
                let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|e| e.to_string())?;
                done += read as u64;
                if last.elapsed() > Duration::from_millis(150) {
                    let _ = send.send(Event::Progress(
                        asset.name.clone(),
                        done,
                        expected.unwrap_or(0),
                    ));
                    last = std::time::Instant::now();
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Err("Download stopped".into());
            }
            if expected.is_some_and(|size| done != size) {
                return Err(format!(
                    "Size mismatch for {}: {done} / {}",
                    asset.name,
                    expected.unwrap_or(0)
                ));
            }
            output.sync_all().map_err(|e| e.to_string())?;
            drop(output);
            fs::rename(&part, target).map_err(|e| e.to_string())?;
            if asset.size.is_none() {
                let directory = target.parent().unwrap().join(".saucecache");
                fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
                let receipt = Receipt {
                    source: asset.browser_download_url.clone(),
                    size: done,
                };
                fs::write(
                    directory.join(format!("{}.json", asset.name)),
                    serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(done)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result
    }
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    source: String,
    size: u64,
}
fn already_saved(asset: &Asset, target: &Path) -> bool {
    let Ok(metadata) = target.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    if let Some(size) = asset.size {
        return metadata.len() == size;
    }
    let path = target
        .parent()
        .unwrap()
        .join(".saucecache")
        .join(format!("{}.json", asset.name));
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice::<Receipt>(&data).ok())
        .is_some_and(|receipt| {
            receipt.source == asset.browser_download_url && receipt.size == metadata.len()
        })
}
fn identifies_project(repo: &Repository, payload: &serde_json::Value) -> bool {
    let Ok(url) = url::Url::parse(&repo.name) else {
        return false;
    };
    let expected = url.path().trim_matches('/');
    let name = match repo.provider {
        Provider::Gitea => payload["full_name"].as_str(),
        Provider::GitLab => payload["path_with_namespace"].as_str(),
        _ => None,
    };
    payload["id"].as_u64().is_some() && name.is_some_and(|name| name.eq_ignore_ascii_case(expected))
}
fn decode_release(
    provider: Provider,
    payload: serde_json::Value,
) -> Result<Option<Release>, String> {
    match provider {
        Provider::Gitea => {
            let releases: Vec<Release> =
                serde_json::from_value(payload).map_err(|e| e.to_string())?;
            Ok(releases.into_iter().find(|r| !r.draft))
        }
        Provider::GitLab => {
            let list = payload.as_array().ok_or("Expected a GitLab release list")?;
            let Some(release) = list
                .iter()
                .find(|r| r["upcoming_release"].as_bool() != Some(true))
            else {
                return Ok(None);
            };
            let mut assets = vec![];
            if let Some(links) = release["assets"]["links"].as_array() {
                for link in links {
                    let Some(raw) = link["direct_asset_url"].as_str().or(link["url"].as_str())
                    else {
                        continue;
                    };
                    let Ok(url) = url::Url::parse(raw) else {
                        continue;
                    };
                    if url.scheme() != "https" {
                        continue;
                    }
                    let filename = url
                        .path_segments()
                        .and_then(|mut parts| parts.rfind(|part| !part.is_empty()))
                        .unwrap_or("");
                    let filename = if safe_filename(filename) && filename.contains('.') {
                        filename
                    } else {
                        link["name"].as_str().unwrap_or(filename)
                    };
                    if safe_filename(filename) {
                        assets.push(Asset {
                            name: filename.into(),
                            size: None,
                            browser_download_url: raw.into(),
                        });
                    }
                }
            }
            Ok(Some(Release {
                tag_name: release["tag_name"]
                    .as_str()
                    .ok_or("Missing release tag")?
                    .into(),
                assets,
                draft: false,
            }))
        }
        Provider::SourceForge => {
            let release = payload
                .get("platform_releases")
                .and_then(|v| v.get("windows"))
                .or(payload.get("release"));
            let Some(release) = release else {
                return Ok(None);
            };
            let filename = release["filename"]
                .as_str()
                .and_then(|s| s.rsplit('/').next())
                .ok_or("Missing SourceForge filename")?;
            if !safe_filename(filename) {
                return Err("Unsafe SourceForge filename".into());
            }
            let raw = release["url"]
                .as_str()
                .ok_or("Missing SourceForge download URL")?;
            let url = if let Some(rest) = raw.strip_prefix("http://") {
                format!("https://{rest}")
            } else {
                raw.into()
            };
            let id = release["sf_file_id"]
                .as_u64()
                .ok_or("Missing SourceForge file identity")?;
            Ok(Some(Release {
                tag_name: format!("recommended-{id}"),
                assets: vec![Asset {
                    name: filename.into(),
                    size: Some(
                        release["bytes"]
                            .as_u64()
                            .ok_or("Missing SourceForge file size")?,
                    ),
                    browser_download_url: url,
                }],
                draft: false,
            }))
        }
        Provider::GitHub => Err("Use the GitHub release adapter".into()),
        Provider::Auto => Err("The release server must be detected first".into()),
    }
}
pub fn safe_filename(name: &str) -> bool {
    !name.is_empty()
        && ![".", ".."].contains(&name)
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && ![
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ]
        .contains(&name.split('.').next().unwrap_or("").to_uppercase().as_str())
}
pub fn tag_folder(tag: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(tag.as_bytes()).collect();
    if safe_filename(&encoded) {
        encoded
    } else {
        format!("release-{encoded}-tag")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct DownloadFixture(PathBuf);
    impl DownloadFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "saucecache-check-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            fs::create_dir_all(root.join("repo/v1")).unwrap();
            fs::write(root.join("repo/v1/old.zip"), b"old").unwrap();
            crate::retention::complete(
                &root.join("repo"),
                "owner/repo",
                "v1",
                vec![crate::retention::DownloadedFile {
                    name: "old.zip".into(),
                    size: 3,
                }],
                false,
                &AtomicBool::new(false),
            )
            .unwrap();
            Self(root)
        }
        fn config(&self, filters: Option<Vec<&str>>) -> Config {
            let mut repo = source::parse("owner/repo", Provider::GitHub).unwrap();
            repo.keep_latest = true;
            repo.suffixes_override =
                filters.map(|filters| filters.into_iter().map(String::from).collect());
            Config {
                repositories: vec![repo],
                output_dir: self.0.to_string_lossy().into(),
                suffixes: vec![".zip".into()],
                ..Default::default()
            }
        }
    }
    impl Drop for DownloadFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const FILTERED_RELEASE: &str = r#"[{"tag_name":"v2","assets":[{"name":"app.ZIP","size":3,"browser_download_url":"https://cdn.example/app.zip"},{"name":"app.msi","size":3,"browser_download_url":"https://cdn.example/app.msi"}]}]"#;
    #[test]
    fn repo_filter_replaces_general_filter_and_cleanup_waits_for_all_files() {
        let directory = DownloadFixture::new();
        let previous = directory.0.join("repo/v1/old.zip");
        let (mut watcher, server) = fixture_observed(
            vec![(200, "", FILTERED_RELEASE), (200, "", "msi")],
            move |_| assert!(previous.exists()),
        );
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(
            directory.config(Some(vec![".msi"])),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("GET /app.msi "));
        assert!(!directory.0.join("repo/v2/app.ZIP").exists());
        assert!(directory.0.join("repo/v2/app.msi").exists());
        assert!(!directory.0.join("repo/v1").exists());
    }
    #[test]
    fn empty_repo_filter_downloads_all_and_only_then_removes_previous_version() {
        let directory = DownloadFixture::new();
        let previous = directory.0.join("repo/v1/old.zip");
        let (mut watcher, server) = fixture_observed(
            vec![
                (200, "", FILTERED_RELEASE),
                (200, "", "zip"),
                (200, "", "msi"),
            ],
            move |_| assert!(previous.exists()),
        );
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(
            directory.config(Some(vec![])),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        server.join().unwrap();
        assert!(directory.0.join("repo/v2/app.ZIP").exists());
        assert!(directory.0.join("repo/v2/app.msi").exists());
        assert!(!directory.0.join("repo/v1").exists());
    }
    #[test]
    fn failed_second_download_keeps_previous_release_and_retry_completes_it() {
        let directory = DownloadFixture::new();
        let (mut watcher, server) = fixture(vec![
            (200, "", FILTERED_RELEASE),
            (200, "", "zip"),
            (500, "", "bad"),
        ]);
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(
            directory.config(Some(vec![])),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        server.join().unwrap();
        assert!(directory.0.join("repo/v1/old.zip").exists());
        assert!(directory.0.join("repo/v2/app.ZIP").exists());
        assert!(!directory.0.join("repo/v2/app.msi").exists());
        let (mut watcher, server) = fixture(vec![(200, "", FILTERED_RELEASE), (200, "", "msi")]);
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(
            directory.config(Some(vec![])),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        server.join().unwrap();
        assert!(!directory.0.join("repo/v1").exists());
    }
    #[test]
    fn no_matches_size_mismatch_and_cancellation_keep_previous_release() {
        let directory = DownloadFixture::new();
        let (mut watcher, server) = fixture(vec![(200, "", FILTERED_RELEASE)]);
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(
            directory.config(Some(vec![".exe"])),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        server.join().unwrap();
        assert!(directory.0.join("repo/v1/old.zip").exists());
        let (mut watcher, server) = fixture(vec![(200, "", FILTERED_RELEASE), (200, "", "short")]);
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(directory.config(None), Arc::new(AtomicBool::new(false)), tx);
        server.join().unwrap();
        assert!(directory.0.join("repo/v1/old.zip").exists());
        assert!(!directory.0.join("repo/v2/app.ZIP").exists());
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (mut watcher, server) = fixture_observed(
            vec![(200, "", FILTERED_RELEASE), (200, "", "zip")],
            move |index| {
                if index == 1 {
                    stop.store(true, Ordering::Relaxed);
                }
            },
        );
        let (tx, _) = std::sync::mpsc::channel();
        watcher.check(directory.config(None), cancel, tx);
        server.join().unwrap();
        assert!(directory.0.join("repo/v1/old.zip").exists());
    }
    #[test]
    fn detects_gitea_api_and_reuses_the_resolved_provider() {
        let (mut watcher, server) = fixture(vec![
            (200, "", r#"{"id":7,"full_name":"team/project"}"#),
            (
                200,
                "ETag: \"v1\"\r\n",
                r#"[{"tag_name":"v1","assets":[]}]"#,
            ),
            (304, "", ""),
        ]);
        let repo = source::parse(
            "https://unlisted-forge.example/team/project",
            Provider::Auto,
        )
        .unwrap();
        assert_eq!(
            watcher.latest_source(&repo).unwrap().unwrap().tag_name,
            "v1"
        );
        assert_eq!(watcher.resolved[&repo.name].provider, Provider::Gitea);
        assert_eq!(
            watcher.latest_source(&repo).unwrap().unwrap().tag_name,
            "v1"
        );
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /api/v1/repos/team/project "));
        assert!(requests[2].contains("if-none-match: \"v1\""));
    }
    #[test]
    fn detects_gitlab_nested_namespaces_after_gitea_probe_fails() {
        let (mut watcher, server) = fixture(vec![
            (404, "", "{}"),
            (
                200,
                "",
                r#"{"id":9,"path_with_namespace":"group/sub/project"}"#,
            ),
            (200, "", r#"[{"tag_name":"v2","assets":{"links":[]}}]"#),
        ]);
        let repo = source::parse(
            "https://unlisted-forge.example/group/sub/project",
            Provider::Auto,
        )
        .unwrap();
        assert_eq!(
            watcher.latest_source(&repo).unwrap().unwrap().tag_name,
            "v2"
        );
        assert_eq!(watcher.resolved[&repo.name].provider, Provider::GitLab);
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("GET /api/v4/projects/group%2Fsub%2Fproject "));
    }
    #[test]
    fn wrong_provider_405_detects_the_right_api_and_emits_a_saved_correction() {
        let (mut watcher, server) = fixture(vec![
            (405, "", ""),
            (200, "", r#"{"id":7,"full_name":"team/project"}"#),
            (200, "", "[]"),
        ]);
        let repo = source::parse(
            "https://unlisted-forge.example/team/project",
            Provider::GitLab,
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        watcher.check(
            Config {
                repositories: vec![repo.clone()],
                ..Default::default()
            },
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|event| matches!(event, Event::SourceResolved { previous, repository } if previous == &repo.name && repository.provider == Provider::Gitea)));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Availability(_, true)))
        );
        let requests = server.join().unwrap();
        assert!(requests[0].contains("/api/v4/projects/"));
        assert!(requests[2].contains("/api/v1/repos/team/project/releases"));
    }
    #[test]
    fn html_pages_are_not_mistaken_for_release_apis_or_missing_repos() {
        let (mut watcher, server) = fixture(vec![
            (
                200,
                "Content-Type: text/html\r\n",
                "<html>Sign in</html>"
            );
            4
        ]);
        let repo = source::parse(
            "https://unlisted-forge.example/team/project",
            Provider::Auto,
        )
        .unwrap();
        assert!(matches!(
            watcher.latest_source(&repo),
            Err(FetchError::Transient(_))
        ));
        assert!(watcher.resolved.is_empty());
        server.join().unwrap();
    }
    #[test]
    fn detects_server_version_before_confirming_a_missing_repo() {
        let (mut watcher, server) = fixture(vec![
            (404, "", "{}"),
            (405, "", ""),
            (200, "", r#"{"version":"17.0.1"}"#),
            (404, "", "{}"),
            (404, "", "{}"),
        ]);
        let repo = source::parse(
            "https://unlisted-forge.example/team/project",
            Provider::Auto,
        )
        .unwrap();
        assert!(matches!(
            watcher.latest_source(&repo),
            Err(FetchError::Unavailable(_))
        ));
        assert_eq!(watcher.resolved[&repo.name].provider, Provider::Gitea);
        server.join().unwrap();
    }
    #[test]
    fn automatic_detection_honors_rate_limits() {
        let (mut watcher, server) = fixture(vec![(429, "Retry-After: 17\r\n", "{}")]);
        let repo = source::parse(
            "https://unlisted-forge.example/team/project",
            Provider::Auto,
        )
        .unwrap();
        assert!(matches!(
            watcher.latest_source(&repo),
            Err(FetchError::RateLimited(60))
        ));
        server.join().unwrap();
    }
    #[test]
    #[ignore = "Uses SAUCECACHE_TEST_REPO_URL for a public self-hosted repo; run explicitly"]
    fn live_automatic_detection_and_small_download() {
        let url = std::env::var("SAUCECACHE_TEST_REPO_URL").expect("Set a public HTTPS repo URL");
        let repo = source::parse(&url, Provider::Auto).unwrap();
        let mut watcher = Watcher::new().unwrap();
        let release = watcher
            .latest_source(&repo)
            .unwrap()
            .expect("Public release");
        let resolved = watcher
            .resolved
            .get(&repo.name)
            .expect("Runtime API detection")
            .clone();
        assert!(matches!(
            resolved.provider,
            Provider::Gitea | Provider::GitLab
        ));
        let asset = release
            .assets
            .iter()
            .filter(|asset| asset.size.is_some_and(|size| size > 0 && size < 256_000))
            .min_by_key(|asset| asset.size)
            .expect("Small public attachment");
        let directory =
            std::env::temp_dir().join(format!("saucecache-auto-live-{}", std::process::id()));
        let target = directory.join(&asset.name);
        let (tx, _) = std::sync::mpsc::channel();
        assert_eq!(
            watcher
                .download(asset, &target, &AtomicBool::new(false), &tx)
                .unwrap(),
            asset.size.unwrap()
        );
        assert!(already_saved(asset, &target));
        fs::remove_file(target).unwrap();
        fs::remove_dir(directory).unwrap();
        // Exercise the legacy wrong selection without putting any hostnames in the program.
        let wrong = source::parse(
            &url,
            if resolved.provider == Provider::Gitea {
                Provider::GitLab
            } else {
                Provider::Gitea
            },
        )
        .unwrap();
        let mut legacy = Watcher::new().unwrap();
        assert!(legacy.latest_source(&wrong).unwrap().is_some());
        assert_eq!(legacy.resolved[&repo.name].provider, resolved.provider);
    }
    #[test]
    #[ignore = "Uses public release endpoints; run explicitly when validating providers"]
    fn live_public_providers() {
        let mut watcher = Watcher::new().unwrap();
        for (url, provider) in [
            ("https://gitlab.com/fdroid/fdroidclient", Provider::GitLab),
            ("https://gitea.com/gitea/tea", Provider::Gitea),
            (
                "https://sourceforge.net/projects/sevenzip",
                Provider::SourceForge,
            ),
        ] {
            let repo = source::parse(url, provider).unwrap();
            let release = watcher
                .latest_source(&repo)
                .unwrap()
                .expect("Public release metadata");
            assert!(!release.assets.is_empty());
            let asset = if provider == Provider::GitLab {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name.ends_with(".asc"))
                    .expect("Small public signature file")
            } else if provider == Provider::Gitea {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name.contains("sha256"))
                    .unwrap_or(&release.assets[0])
            } else {
                &release.assets[0]
            };
            if asset.size.is_some_and(|size| size > 3_000_000) {
                let mut response = watcher
                    .client
                    .get(&asset.browser_download_url)
                    .header("Range", "bytes=0-3")
                    .send()
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                assert!(
                    !response
                        .headers()
                        .get("content-type")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("text/html")
                );
                let mut bytes = [0; 4];
                response.read_exact(&mut bytes).unwrap();
            } else {
                let directory = std::env::temp_dir().join(format!(
                    "saucecache-live-{}-{:?}",
                    std::process::id(),
                    provider
                ));
                let target = directory.join(&asset.name);
                let (tx, _) = std::sync::mpsc::channel();
                let size = watcher
                    .download(asset, &target, &AtomicBool::new(false), &tx)
                    .unwrap();
                assert_eq!(target.metadata().unwrap().len(), size);
                assert!(already_saved(asset, &target));
                fs::remove_file(&target).unwrap();
                if asset.size.is_none() {
                    fs::remove_file(
                        directory
                            .join(".saucecache")
                            .join(format!("{}.json", asset.name)),
                    )
                    .unwrap();
                    fs::remove_dir(directory.join(".saucecache")).unwrap();
                }
                fs::remove_dir(directory).unwrap();
            }
        }
    }
    #[test]
    fn gitlab_assets_use_real_filename_without_inventing_size() {
        let payload = serde_json::json!([{"tag_name":"future","upcoming_release":true},{"tag_name":"v1","assets":{"links":[{"name":"Windows build","direct_asset_url":"https://cdn.example.org/app.zip"}]}}]);
        let release = decode_release(Provider::GitLab, payload).unwrap().unwrap();
        assert_eq!(release.tag_name, "v1");
        assert_eq!(release.assets[0].name, "app.zip");
        assert_eq!(release.assets[0].size, None);
    }
    #[test]
    fn gitea_skips_drafts() {
        let payload = serde_json::json!([{"tag_name":"draft","draft":true},{"tag_name":"nightly","prerelease":true,"assets":[{"name":"app.zip","size":123,"browser_download_url":"https://forge.example.org/app.zip"}]}]);
        let release = decode_release(Provider::Gitea, payload).unwrap().unwrap();
        assert_eq!(release.tag_name, "nightly");
        assert_eq!(release.assets[0].size, Some(123));
    }
    #[test]
    fn sourceforge_prefers_windows_and_preserves_signed_link() {
        let file = serde_json::json!({"filename":"/app/1.0/setup.exe","bytes":42,"sf_file_id":123,"url":"http://downloads.sourceforge.net/project/app/setup.exe?ts=signed"});
        let release=decode_release(Provider::SourceForge,serde_json::json!({"platform_releases":{"windows":file},"release":{"filename":"/linux.tar.gz"}})).unwrap().unwrap();
        assert_eq!(release.assets[0].name, "setup.exe");
        assert_eq!(release.assets[0].size, Some(42));
        assert!(
            release.assets[0]
                .browser_download_url
                .starts_with("https://")
        );
        assert!(
            release.assets[0]
                .browser_download_url
                .ends_with("?ts=signed")
        );
        assert_eq!(release.tag_name, "recommended-123");
    }
    #[test]
    fn unknown_size_requires_completion_receipt() {
        let folder =
            std::env::temp_dir().join(format!("saucecache-receipt-test-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let target = folder.join("app.zip");
        fs::write(&target, b"abc").unwrap();
        let asset = Asset {
            name: "app.zip".into(),
            size: None,
            browser_download_url: "https://cdn.example.org/app.zip".into(),
        };
        assert!(!already_saved(&asset, &target));
        fs::create_dir_all(folder.join(".saucecache")).unwrap();
        fs::write(
            folder.join(".saucecache/app.zip.json"),
            serde_json::to_vec(&Receipt {
                source: asset.browser_download_url.clone(),
                size: 3,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(already_saved(&asset, &target));
        fs::write(&target, b"bad length").unwrap();
        assert!(!already_saved(&asset, &target));
        fs::remove_file(&target).unwrap();
        fs::remove_file(folder.join(".saucecache/app.zip.json")).unwrap();
        fs::remove_dir(folder.join(".saucecache")).unwrap();
        fs::remove_dir(folder).unwrap();
    }
    fn fixture(
        responses: Vec<(u16, &'static str, &'static str)>,
    ) -> (Watcher, std::thread::JoinHandle<Vec<String>>) {
        fixture_observed(responses, |_| {})
    }
    fn fixture_observed(
        responses: Vec<(u16, &'static str, &'static str)>,
        observe: impl Fn(usize) + Send + 'static,
    ) -> (Watcher, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = vec![];
            for (index, (code, headers, body)) in responses.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                requests.push(String::from_utf8_lossy(&bytes).to_string());
                observe(index);
                write!(stream,"HTTP/1.1 {code} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",body.len()).unwrap();
            }
            requests
        });
        let mut watcher = Watcher::new().unwrap();
        watcher.api_base = format!("http://{address}");
        watcher.source_test_origin = Some(format!("http://{address}"));
        (watcher, server)
    }
    #[test]
    fn missing_repository_is_confirmed() {
        let (mut watcher, server) = fixture(vec![(404, "", "{}"), (404, "", "{}")]);
        assert!(matches!(
            watcher.latest("owner/repo"),
            Err(FetchError::Unavailable(_))
        ));
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("GET /repos/owner/repo HTTP/1.1"));
    }
    #[test]
    fn missing_releases_is_not_missing_repository() {
        let (mut watcher, server) = fixture(vec![(404, "", "{}"), (200, "", "{}")]);
        assert!(matches!(
            watcher.latest("owner/repo"),
            Err(FetchError::Transient(_))
        ));
        server.join().unwrap();
    }
    #[test]
    fn prerelease_and_etag() {
        let body = r#"[{"tag_name":"draft","draft":true},{"tag_name":"nightly","prerelease":true,"draft":false}]"#;
        let (mut watcher, server) = fixture(vec![(200, "ETag: \"v1\"\r\n", body), (304, "", "")]);
        assert_eq!(
            watcher.latest("owner/repo").unwrap().unwrap().tag_name,
            "nightly"
        );
        assert_eq!(
            watcher.latest("owner/repo").unwrap().unwrap().tag_name,
            "nightly"
        );
        assert!(
            server.join().unwrap()[1]
                .to_lowercase()
                .contains("if-none-match: \"v1\"")
        );
    }
    #[test]
    fn rate_limit_is_not_unavailable() {
        let (mut watcher, server) = fixture(vec![(429, "Retry-After: 90\r\n", "{}")]);
        assert!(matches!(
            watcher.latest("owner/repo"),
            Err(FetchError::RateLimited(90))
        ));
        server.join().unwrap();
    }
    #[test]
    fn empty_releases_is_healthy() {
        let (mut watcher, server) = fixture(vec![(200, "", "[]")]);
        assert!(watcher.latest("owner/repo").unwrap().is_none());
        server.join().unwrap();
    }
    #[test]
    fn paths() {
        for name in ["../evil.exe", "C:\\evil", "CON.txt", ".", "file."] {
            assert!(!safe_filename(name));
        }
        assert!(safe_filename("app.zip"));
        assert!(!tag_folder("../nightly").contains('/'));
    }
}
