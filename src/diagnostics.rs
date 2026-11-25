//! Bounded, local diagnostics. Never serialize settings, credentials, or command lines.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const MAX_LOG_BYTES: u64 = 512 * 1024;
const MAX_ENTRY_BYTES: usize = 8 * 1024;
static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

struct Log {
    directory: PathBuf,
    limit: u64,
}
impl Log {
    fn write(&self, level: &str, message: &str) -> std::io::Result<()> {
        let mut end = message.len().min(MAX_ENTRY_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let message = message[..end].replace('\r', "\\r").replace('\n', "\\n");
        let line = format!(
            "{} [{level}] {message}\n",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        let path = self.directory.join("saucecache.log");
        if fs::metadata(&path).map_or(0, |m| m.len()) + line.len() as u64 > self.limit {
            let previous = self.directory.join("saucecache.log.1");
            let oldest = self.directory.join("saucecache.log.2");
            if oldest.exists() {
                fs::remove_file(&oldest)?;
            }
            if previous.exists() {
                fs::rename(&previous, &oldest)?;
            }
            if path.exists() {
                fs::rename(&path, &previous)?;
            }
        }
        let mut output = OpenOptions::new().create(true).append(true).open(path)?;
        output.write_all(line.as_bytes())?;
        if matches!(level, "panic" | "fatal") {
            output.sync_data()?;
        }
        Ok(())
    }
}

pub fn record(level: &str, message: &str) {
    if let Some(log) = LOG.get() {
        // A diagnostic write failure must never take down the application.
        let guard = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = guard.write(level, message);
    }
}

pub struct Session {
    marker: Option<PathBuf>,
}
impl Session {
    pub fn finish(self) {
        record("session", "Normal shutdown");
        if let Some(marker) = self.marker {
            let _ = fs::remove_file(marker);
        }
    }
}

pub fn start(config_directory: &Path) -> Session {
    let directory = config_directory.join("logs");
    if fs::create_dir_all(&directory).is_err() {
        return Session { marker: None };
    }
    if LOG
        .set(Mutex::new(Log {
            directory: directory.clone(),
            limit: MAX_LOG_BYTES,
        }))
        .is_ok()
    {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let current = std::thread::current();
            record(
                "panic",
                &format!("thread={}: {info}", current.name().unwrap_or("unnamed")),
            );
            record(
                "panic",
                &format!("{}", std::backtrace::Backtrace::force_capture()),
            );
            original(info);
        }));
    }
    let marker = directory.join("session-active.txt");
    if marker.exists() {
        record(
            "session",
            "Previous session ended unexpectedly (crash, forced termination, or power loss)",
        );
    }
    record(
        "session",
        &format!(
            "Starting SauceCache {} pid={}",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ),
    );
    heartbeat(false, false, false);
    let marker = fs::write(
        &marker,
        format!(
            "version={}\nstarted_utc={}\npid={}\n",
            env!("CARGO_PKG_VERSION"),
            chrono::Utc::now().to_rfc3339(),
            std::process::id()
        ),
    )
    .ok()
    .map(|_| marker);
    Session { marker }
}

pub fn heartbeat(hidden: bool, watching: bool, busy: bool) {
    let memory = process_memory().map_or_else(
        || "memory=unavailable".into(),
        |(private, working)| format!("private_bytes={private} working_set_bytes={working}"),
    );
    record(
        "health",
        &format!("hidden={hidden} watching={watching} busy={busy} {memory}"),
    );
}

#[cfg(windows)]
pub(crate) fn process_memory() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::GetCurrentProcess,
    };
    let mut memory: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    memory.cb = std::mem::size_of_val(&memory) as u32;
    let result = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut memory as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            memory.cb,
        )
    };
    (result != 0).then_some((memory.PrivateUsage as u64, memory.WorkingSetSize as u64))
}

#[cfg(not(windows))]
pub(crate) fn process_memory() -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "saucecache-diagnostics-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn diagnostics_rotate_bound_entries_and_preserve_unicode() {
        let fixture = Fixture::new();
        let log = Log {
            directory: fixture.0.clone(),
            limit: 512,
        };
        for index in 0..50 {
            log.write("test", &format!("line {index} — diagnostic data"))
                .unwrap();
        }
        let paths = fs::read_dir(&fixture.0)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(paths.len(), 3);
        for entry in paths {
            assert!(entry.metadata().unwrap().len() <= 512);
        }
        assert!(
            fs::read_to_string(fixture.0.join("saucecache.log"))
                .unwrap()
                .contains("line 49")
        );
        log.write("test", &"🙂".repeat(MAX_ENTRY_BYTES)).unwrap();
        let text = fs::read_to_string(fixture.0.join("saucecache.log")).unwrap();
        assert!(!text.contains('�'));
        assert!(text.len() < MAX_ENTRY_BYTES + 100);
    }
    #[test]
    fn diagnostics_io_errors_are_returned_instead_of_panicking() {
        let fixture = Fixture::new();
        let log = Log {
            directory: fixture.0.join("missing"),
            limit: MAX_LOG_BYTES,
        };
        assert!(log.write("test", "message").is_err());
    }
    #[test]
    #[ignore = "Installs a process-wide panic hook; run alone explicitly"]
    fn panic_and_previous_session_are_recorded_and_normal_shutdown_clears_marker() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.0.join("logs")).unwrap();
        let marker = fixture.0.join("logs/session-active.txt");
        fs::write(&marker, b"previous run").unwrap();
        let session = start(&fixture.0);
        assert!(std::panic::catch_unwind(|| panic!("diagnostic fixture panic")).is_err());
        assert!(marker.exists());
        let text = fs::read_to_string(fixture.0.join("logs/saucecache.log")).unwrap();
        assert!(text.contains("Previous session ended unexpectedly"));
        assert!(text.contains("diagnostic fixture panic"));
        assert!(process_memory().is_some());
        session.finish();
        assert!(!marker.exists());
        assert!(
            fs::read_to_string(fixture.0.join("logs/saucecache.log"))
                .unwrap()
                .contains("Normal shutdown")
        );
    }
}
