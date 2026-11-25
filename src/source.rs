//! Verified public release sources. No generic web scraping or guessed APIs.
use crate::config::Repository;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Auto,
    #[default]
    GitHub,
    GitLab,
    Gitea,
    SourceForge,
}
impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "Auto-detect",
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Gitea => "Gitea / Forgejo",
            Self::SourceForge => "SourceForge",
        })
    }
}
pub const PROVIDERS: [Provider; 5] = [
    Provider::Auto,
    Provider::GitHub,
    Provider::GitLab,
    Provider::Gitea,
    Provider::SourceForge,
];
pub fn parse(input: &str, selected: Provider) -> Result<Repository, String> {
    let input = input.trim();
    if !input.contains("://")
        && !input.contains('.')
        && matches!(selected, Provider::Auto | Provider::GitHub)
    {
        return Ok(Repository {
            name: crate::config::parse_repository(input)?,
            unavailable: false,
            provider: Provider::GitHub,
            ..Default::default()
        });
    }
    // Plain owner/repo entries (including dots) retain the established GitHub interpretation.
    let known_host = [
        "github.com/",
        "www.github.com/",
        "gitlab.com/",
        "gitea.com/",
        "codeberg.org/",
        "sourceforge.net/",
        "www.sourceforge.net/",
    ];
    if !input.contains("://")
        && input
            .split('/')
            .next()
            .is_some_and(|part| part.contains('.'))
        && input.split('/').count() > 2
        && !known_host
            .iter()
            .any(|host| input.to_lowercase().starts_with(host))
    {
        return Err("Unsupported host; use a supported HTTPS repo URL".into());
    }
    let value = if input.contains("://") {
        input.to_owned()
    } else if known_host
        .iter()
        .any(|host| input.to_lowercase().starts_with(host))
    {
        format!("https://{input}")
    } else if matches!(selected, Provider::Auto | Provider::GitHub) {
        return Ok(Repository {
            name: crate::config::parse_repository(input)?,
            unavailable: false,
            provider: Provider::GitHub,
            ..Default::default()
        });
    } else {
        return Err("Enter a full HTTPS repo URL for this provider".into());
    };
    let url = url::Url::parse(&value).map_err(|_| "Invalid repo URL")?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("Use an HTTPS URL without embedded credentials".into());
    }
    let host = url.host_str().ok_or("Missing hostname")?;
    let detected = match host {
        "github.com" | "www.github.com" => Some(Provider::GitHub),
        "gitlab.com" => Some(Provider::GitLab),
        "gitea.com" | "codeberg.org" => Some(Provider::Gitea),
        "sourceforge.net" | "www.sourceforge.net" => Some(Provider::SourceForge),
        _ => None,
    };
    let provider = if matches!(selected, Provider::Auto | Provider::GitHub) {
        match detected {
            Some(provider) => provider,
            None if selected == Provider::Auto => Provider::Auto,
            None => {
                return Err(
                    "Unsupported GitHub host. Choose Auto-detect for a self-hosted server.".into(),
                );
            }
        }
    } else {
        if detected.is_some_and(|p| p != selected) {
            return Err("Selected provider does not match the URL".into());
        }
        selected
    };
    if provider == Provider::GitHub {
        return Ok(Repository {
            name: crate::config::parse_repository(&value)?,
            unavailable: false,
            provider,
            ..Default::default()
        });
    }
    let path = url.path().split("/-/").next().unwrap_or_default();
    let mut parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if provider == Provider::SourceForge {
        if !["sourceforge.net", "www.sourceforge.net"].contains(&host)
            || parts.len() < 2
            || !["projects", "p"].contains(&parts[0])
        {
            return Err("Use https://sourceforge.net/projects/project-name".into());
        }
        parts = vec!["projects", parts[1]];
    } else if provider == Provider::Gitea {
        if parts.len() < 2 {
            return Err("Use an owner/repo URL".into());
        }
        parts.truncate(2);
    }
    if parts.len() < 2 {
        return Err("Enter the complete repo path".into());
    }
    let mut parts: Vec<String> = parts.into_iter().map(String::from).collect();
    if let Some(last) = parts.last_mut() {
        *last = last.trim_end_matches(".git").to_owned();
    }
    if parts.iter().any(|p| {
        p.is_empty()
            || p == "."
            || p == ".."
            || !p
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    }) {
        return Err("Unsupported repo path".into());
    }
    Ok(Repository {
        name: format!("{}/{}", url.origin().ascii_serialization(), parts.join("/")),
        unavailable: false,
        provider,
        ..Default::default()
    })
}
pub fn browser_url(repo: &Repository) -> String {
    if repo.provider == Provider::GitHub {
        format!("https://github.com/{}", repo.name)
    } else {
        repo.name.clone()
    }
}
pub struct Spec {
    pub releases: String,
    pub verify: String,
    pub storage: PathBuf,
}
pub fn spec(repo: &Repository) -> Result<Spec, String> {
    if repo.provider == Provider::Auto {
        return Err("The release server must be detected first".into());
    }
    if repo.provider == Provider::GitHub {
        return Ok(Spec {
            releases: format!(
                "https://api.github.com/repos/{}/releases?per_page=10",
                repo.name
            ),
            verify: format!("https://api.github.com/repos/{}", repo.name),
            storage: PathBuf::from(repo.name.split('/').next_back().unwrap_or("repo")),
        });
    }
    let url = url::Url::parse(&repo.name).map_err(|_| "Invalid saved repo URL")?;
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_matches('/');
    let storage = PathBuf::from(url.host_str().unwrap_or("host")).join(path);
    Ok(match repo.provider {
        Provider::GitLab => {
            let id: String = url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
            Spec {
                releases: format!(
                    "{origin}/api/v4/projects/{id}/releases?per_page=10&order_by=released_at&sort=desc"
                ),
                verify: format!("{origin}/api/v4/projects/{id}"),
                storage,
            }
        }
        Provider::Gitea => Spec {
            releases: format!("{origin}/api/v1/repos/{path}/releases?limit=10"),
            verify: format!("{origin}/api/v1/repos/{path}"),
            storage,
        },
        Provider::SourceForge => Spec {
            releases: format!("{origin}/{path}/best_release.json"),
            verify: format!("{origin}/{path}/"),
            storage,
        },
        Provider::GitHub | Provider::Auto => unreachable!(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_accepts_arbitrary_https_hosts_without_a_hostname_allowlist() {
        let repo = parse(
            "https://some-new-forge.example/team/project/releases",
            Provider::Auto,
        )
        .unwrap();
        assert_eq!(repo.provider, Provider::Auto);
        assert!(repo.name.starts_with("https://some-new-forge.example/"));
        assert_eq!(
            parse("owner/repo", Provider::Auto).unwrap().provider,
            Provider::GitHub
        );
        assert!(
            parse(
                "https://user:secret@forge.example/team/project",
                Provider::Auto
            )
            .is_err()
        );
    }
    #[test]
    fn normalize() {
        assert_eq!(
            parse(
                "https://gitlab.com/group/sub/repo/-/releases/v1",
                Provider::GitHub
            )
            .unwrap()
            .name,
            "https://gitlab.com/group/sub/repo"
        );
        assert_eq!(
            parse("owner/repo", Provider::GitHub).unwrap().name,
            "owner/repo"
        );
        assert_eq!(
            parse("https://codeberg.org/user/repo/releases", Provider::GitHub)
                .unwrap()
                .provider,
            Provider::Gitea
        );
        assert!(parse("https://bitbucket.org/team/repo", Provider::GitHub).is_err());
    }
    #[test]
    fn self_hosted_explicit() {
        assert!(parse("https://forge.example.org/user/repo", Provider::GitHub).is_err());
        let repo = parse("https://forge.example.org/user/repo", Provider::Gitea).unwrap();
        assert!(
            spec(&repo)
                .unwrap()
                .releases
                .contains("/api/v1/repos/user/repo/releases")
        );
    }
}
