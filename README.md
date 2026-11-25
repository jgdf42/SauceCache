# SauceCache

A release pantry for your favorite repos. SauceCache is a Windows app that watches for new releases and downloads their files into folders organized by repo and version.

<img width="1986" height="1768" alt="saucecache" src="https://github.com/user-attachments/assets/66ba71c5-bc91-4e37-8585-b179758d9843" />

## Key features

- Scheduled checks or a manual **Check now**.
- Public releases from GitHub, GitLab, Gitea/Forgejo (including Codeberg), and SourceForge.
- Automatic detection of public self-hosted GitLab and Gitea/Forgejo servers.
- File extension filters, per-repo overrides, and automatic skipping of files already downloaded.
- Per-repo version retention: keep all versions or remove older tracked downloads after a successful update.
- Status badges for unavailable repos, with records kept for future rechecks.
- System tray support, light/dark themes, and settings that save automatically.
- Optional MQTT notifications for app status, repo counts, and downloads.
- Configuration backup and restore.

SourceForge downloads one recommended file. Private repos and servers installed under a subpath aren't supported.

## Get started

1. Unzip the Windows package and run **SauceCache.exe**.
2. Click **Add repos** and paste full HTTPS URLs, one per line. Bare `owner/repo` entries use GitHub.
3. Choose a download folder and check interval.
4. Click **Start watching**, or **Check now** for a single check.

Fresh installs start with no repos or download folder, and MQTT disabled. No additional runtime is required for the packaged app.

Settings are available through the cog button. Minimizing sends the app to the tray; use the tray menu to exit.

MQTT passwords are stored in Windows Credential Manager. **Configuration backup ZIPs can contain your MQTT password and are not encrypted—keep them private.**

## Build from source

Requires Windows, Rust 1.88+, and a Windows linker/resource compiler (Visual Studio Build Tools or a matching LLVM-MinGW toolchain).

```powershell
cargo build --release --locked
```

## Credits

Version 0.1.2. Thanks to [SkylerWallace / sk-iced](https://github.com/SkylerWallace/sk-iced) for the UI toolkit and design assistance.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [licenses/](licenses/) for third-party notices.
