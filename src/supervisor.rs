//! Keeps Tempo alive. Started from the Run key or Start menu, the exe first acts as a tiny
//! supervisor that runs the real app (`--child`) and restarts it if it dies unexpectedly. Quit and
//! updates end it normally. The object names are the ones Tempo 1.x used, so the two never run at once.

use crate::config;
use crate::system::{self, wide};
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows_core::PCWSTR;

const MAX_CRASHES: usize = 5;
const CRASH_WINDOW_MS: u64 = 10 * 60 * 1000;

/// A named object of this instance (test copies have their own).
pub fn name(object: &str) -> Vec<u16> {
    wide(&format!(r"Local\Tempo.{object}{}", config::instance_tag()))
}

pub fn signal(event: &str) {
    let n = name(event);
    unsafe {
        let _ = AllowSetForegroundWindow(u32::MAX /* ASFW_ANY */);
        if let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(n.as_ptr())) {
            let _ = SetEvent(h);
        }
    }
}

/// Takes the named mutex, waiting up to `ms`. `None` if another process keeps it.
pub fn lock(object: &str, ms: u32) -> Option<HANDLE> {
    let n = name(object);
    unsafe {
        let mutex = CreateMutexW(None, false, PCWSTR(n.as_ptr())).ok()?;
        let got = WaitForSingleObject(mutex, ms);
        // abandoned: the previous owner died, so it's ours now
        (got == WAIT_OBJECT_0 || got == WAIT_ABANDONED).then_some(mutex)
    }
}

pub fn run(args: &[String]) {
    // After an update, the new version waits for the old one to finish closing.
    if let Some(old) = args.iter().skip_while(|a| *a != "--wait-for").nth(1).and_then(|a| a.parse().ok()) {
        system::wait_for_exit(old, 15_000);
    }
    let updated = args.iter().any(|a| a == "--updated");
    let Some(_mutex) = lock("Supervisor", if updated { 10_000 } else { 0 }) else {
        // Already running: ask it to open settings (or show the pop-up).
        signal(if args.iter().any(|a| a == "--preview") { "Preview" } else { "ShowSettings" });
        return;
    };
    let mut child_args = vec!["--child".to_string()];
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        if a == "--wait-for" {
            rest.next();
        } else {
            child_args.push(a.clone());
        }
    }
    let mut crashes: Vec<u64> = Vec::new();
    loop {
        let status = std::process::Command::new(system::exe_path()).args(&child_args).spawn().and_then(|mut c| c.wait());
        let code = match status {
            Ok(s) => s.code().unwrap_or(-1),
            Err(e) => {
                config::log(&format!("Supervisor could not start Tempo: {e}"));
                break;
            }
        };
        if code == 0 {
            break; // quit, update hand-over, or Windows signing out
        }
        config::log(&format!("Tempo stopped unexpectedly (exit code 0x{code:08X}); restarting it."));
        let now = system::now_ms();
        crashes.push(now);
        crashes.retain(|t| now - t <= CRASH_WINDOW_MS);
        if crashes.len() >= MAX_CRASHES {
            config::log(&format!("Tempo crashed {MAX_CRASHES} times in 10 minutes; not restarting it again."));
            break;
        }
        child_args = ["--child", "--background", "--restarted"].map(String::from).to_vec(); // never reopen settings on a restart
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
}
