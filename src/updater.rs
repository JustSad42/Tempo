//! Self-update from GitHub releases. A release carries Tempo.exe and Tempo.exe.sha256 (the same
//! names Tempo 1.x looks for, so installed copies move to this version by themselves). The download
//! is checked, kept in memory, and swapped in while the pop-up is hidden and no key is held.

use crate::{Event, post};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::Duration;

pub const REPO: &str = "JustSad42/Tempo";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A verified new version waiting to be installed.
pub static READY: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);
/// One line for the settings window.
pub static STATUS: Mutex<String> = Mutex::new(String::new());
static BUSY: AtomicBool = AtomicBool::new(false);

fn set(status: impl Into<String>) {
    *STATUS.lock().unwrap() = status.into();
    post(Event::UpdateStatus);
}

pub fn newer(remote: &str, local: &str) -> bool {
    let parse = |v: &str| {
        let mut n = v.trim_start_matches(['v', 'V']).split('.').map(|x| x.parse::<u32>().ok());
        Some((n.next()??, n.next().unwrap_or(Some(0))?, n.next().unwrap_or(Some(0))?))
    };
    matches!((parse(remote), parse(local)), (Some(r), Some(l)) if r > l)
}

/// Only an installed copy named Tempo.exe updates itself (not a dev build or a test copy).
pub fn can_self_update() -> bool {
    let exe = crate::system::exe_path().to_string_lossy().to_lowercase();
    exe.ends_with("tempo.exe") && !exe.contains(r"\target\") && crate::config::instance_tag().is_empty()
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        // Windows' own TLS, so no TLS library is bundled in the exe.
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build())
        .build()
        .into()
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut r = agent().get(url).header("User-Agent", "Tempo-updater").call().map_err(|e| e.to_string())?;
    r.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| e.to_string())
}

/// Never install an unverified file.
pub fn verify(bytes: &[u8], expected: &str) -> bool {
    expected.len() == 64 && Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>().eq_ignore_ascii_case(expected)
}

fn fetch() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut r = match agent().get(&url).header("User-Agent", "Tempo-updater").header("Accept", "application/vnd.github+json").call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok("No releases published yet.".into()),
        Err(e) => return Err(e.to_string()),
    };
    let release: Release = r.body_mut().read_json().map_err(|e| e.to_string())?;
    if !newer(&release.tag_name, VERSION) {
        return Ok(format!("You have the latest version ({VERSION})."));
    }
    let latest = release.tag_name.trim_start_matches(['v', 'V']).to_string();
    let asset = |name: &str| release.assets.iter().find(|a| a.name == name);
    let Some(exe) = asset("Tempo.exe") else { return Ok(format!("Version {latest} is out, but has no download yet.")) };
    if !can_self_update() {
        return Ok(format!("Version {latest} is available on GitHub."));
    }
    // The expected hash: the .sha256 file, else GitHub's own digest.
    let expected = match asset("Tempo.exe.sha256") {
        Some(a) => String::from_utf8_lossy(&get(&a.browser_download_url, 4096)?).split_whitespace().next().unwrap_or("").to_string(),
        None => exe.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).unwrap_or("").to_string(),
    };
    if expected.is_empty() {
        return Ok(format!("Version {latest} has no checksum, so it wasn't installed."));
    }
    set(format!("Downloading version {latest}…"));
    let bytes = get(&exe.browser_download_url, 100 << 20)?;
    if !verify(&bytes, &expected) {
        return Ok("The download didn't match its checksum, so it wasn't installed.".into());
    }
    *READY.lock().unwrap() = Some((latest.clone(), bytes));
    Ok(format!("Version {latest} is ready. It installs in a moment."))
}

/// Looks for a newer release and downloads it, on its own thread.
pub fn check(manual: bool) {
    if READY.lock().unwrap().is_some() || BUSY.swap(true, SeqCst) {
        return;
    }
    set("Checking for updates…");
    std::thread::spawn(move || {
        match fetch() {
            Ok(s) => set(s),
            Err(e) => set(if manual { format!("Couldn't check for updates: {e}") } else { "Couldn't check for updates.".into() }),
        }
        BUSY.store(false, SeqCst);
        if READY.lock().unwrap().is_some() {
            post(Event::UpdateReady);
        }
    });
}

/// Puts `bytes` at `exe`, keeping the running file as `<name>.old`.
pub fn replace(exe: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?; // Windows allows renaming a running exe
    std::fs::write(exe, bytes).inspect_err(|_| {
        let _ = std::fs::rename(&old, exe);
    })
}

/// Swaps in the downloaded version and starts it. True if the caller should now exit.
pub fn apply_and_restart() -> bool {
    let Some((version, bytes)) = READY.lock().unwrap().take() else { return false };
    crate::config::log(&format!("Installing version {version}."));
    let exe = crate::system::exe_path();
    if replace(&exe, &bytes).is_err() {
        set("Couldn't install the update (is the folder read-only?).");
        return false;
    }
    std::process::Command::new(&exe).args(["--background", "--updated", "--wait-for", &std::process::id().to_string()]).spawn().is_ok()
}

/// Removes what an update left behind. The old exe may still be closing, so retry, off the UI thread.
pub fn clean_up() {
    std::thread::spawn(|| {
        let exe = crate::system::exe_path();
        for _ in 0..20 {
            // ".new" is what Tempo 1.x downloaded next to itself.
            let gone = [exe.with_extension("exe.old"), exe.with_extension("exe.new")].iter().all(|p| !p.exists() || std::fs::remove_file(p).is_ok());
            if gone {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_checksums() {
        assert!(newer("v2.0.0", "1.2.1"));
        assert!(newer("1.10", "1.9.9"));
        assert!(!newer("v2.0.0", "2.0.0"));
        assert!(!newer("nonsense", "1.0.0"));
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify(b"abc", abc) && verify(b"abc", &abc.to_uppercase()));
        assert!(!verify(b"abd", abc) && !verify(b"abc", ""));
    }

    #[test]
    fn replace_keeps_the_old_exe() {
        let dir = std::env::temp_dir().join(format!("tempo-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Tempo.exe");
        std::fs::write(&exe, "one").unwrap();
        replace(&exe, b"two").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"two");
        assert_eq!(std::fs::read(dir.join("Tempo.exe.old")).unwrap(), b"one");
        let _ = std::fs::remove_dir_all(dir);
    }
}
