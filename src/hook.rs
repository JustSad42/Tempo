//! Catches the keyboard's media keys before Windows does. The hook runs on its own thread with its
//! own message loop so it always answers at once (Windows silently removes hooks that are slow).

use crate::logic::{HookState, Key, NON_REPORTING_PLAYERS, PassThrough};
use crate::system::{self, now_ms};
use crate::{Event, apps, post};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_HIGHEST};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Written by the UI thread (and the audio watch), read by the hook.
pub static STATE: Mutex<Option<HookState>> = Mutex::new(None);
/// The apps Windows reports, for the audio watch.
pub static APPS: Mutex<Vec<apps::AudioId>> = Mutex::new(Vec::new());
/// When the hook last received any media key.
pub static LAST_SEEN: AtomicU64 = AtomicU64::new(0);
/// Key presses handed to other players since the UI thread last counted them.
pub static PASSED: AtomicU32 = AtomicU32::new(0);
static THREAD: AtomicU32 = AtomicU32::new(0);
/// Test only: the hook stops noticing keys, as if Windows had dropped it, but still keeps them.
pub static BLIND: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct KeyState {
    down: [bool; 4],
    captured: [bool; 4],
    pass: PassThrough,
}

thread_local!(static KEYS: std::cell::RefCell<KeyState> = Default::default());

fn map(vk: u16) -> Option<(usize, Key)> {
    match VIRTUAL_KEY(vk) {
        VK_MEDIA_PLAY_PAUSE => Some((0, Key::PlayPause)),
        VK_MEDIA_NEXT_TRACK => Some((1, Key::Next)),
        VK_MEDIA_PREV_TRACK => Some((2, Key::Previous)),
        VK_MEDIA_STOP => Some((3, Key::Stop)),
        _ => None,
    }
}

unsafe extern "system" fn callback(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 {
            let info = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            if let Some((i, key)) = map(info.vkCode as u16) {
                if BLIND.load(Relaxed) {
                    return LRESULT(1);
                }
                LAST_SEEN.store(now_ms(), Relaxed);
                let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let keep = KEYS.with_borrow_mut(|k| {
                    let repeat = down && k.down[i];
                    if down && !repeat {
                        let state = STATE.lock().ok().and_then(|s| *s);
                        k.captured[i] = match state {
                            None => false,
                            Some(s) => match k.pass.capture(&s, now_ms(), system::process_alive) {
                                Ok(()) => true,
                                Err(handed) => {
                                    if handed {
                                        PASSED.fetch_add(1, Relaxed);
                                    }
                                    false
                                }
                            },
                        };
                    }
                    k.down[i] = down;
                    if k.captured[i] && !repeat {
                        post(Event::Key(key, down));
                    }
                    k.captured[i]
                });
                if keep {
                    return LRESULT(1); // swallow: Windows' own flyout and other apps never see it
                }
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

pub fn start() {
    let (ready, installed) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(callback), module, 0);
        THREAD.store(GetCurrentThreadId(), Relaxed);
        let _ = ready.send(hook.as_ref().err().map(|e| e.to_string()));
        let Ok(hook) = hook else { return };
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
        let _ = UnhookWindowsHookEx(hook);
    });
    match installed.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(None) => {}
        Ok(Some(e)) => crate::config::log(&format!("Keyboard hook failed: {e}")),
        Err(_) => crate::config::log("Keyboard hook failed: no answer"),
    }
}

pub fn stop() {
    let thread = THREAD.swap(0, Relaxed);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

// ---------- second pair of eyes ----------

/// Raw input sees every key press whether or not the hook does. Media keys arriving here that the
/// hook never saw mean Windows dropped it.
pub fn watch_raw_input(window: HWND) {
    let device = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: window };
    if unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }.is_err() {
        crate::config::log("Raw input watch unavailable.");
    }
}

/// True if this `WM_INPUT` is a media key going down (in a test copy, F24 stands in for one).
pub fn is_media_key_down(lp: LPARAM, test: bool) -> bool {
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    let read = unsafe { GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, Some(&mut raw as *mut _ as _), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
    if read == u32::MAX || raw.header.dwType != RIM_TYPEKEYBOARD.0 {
        return false;
    }
    let k = unsafe { raw.data.keyboard };
    let down = k.Flags as u32 & 1 == 0;
    down && (map(k.VKey).is_some() || (test && k.VKey == VK_F24.0))
}

// ---------- players Windows can't see ----------

/// Once a second, notes whether a player that doesn't report to Windows (VLC 3…) is making sound.
pub fn start_audio_watch() {
    std::thread::spawn(|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
        let (mut open, mut next_scan) = (false, 0);
        loop {
            let wanted = STATE.lock().unwrap().is_some_and(|s| s.pass_through && !s.target_playing);
            // Listing processes is cheap, querying every audio session is not: only do the latter
            // while one of those players is open (checked every 10 s).
            if wanted && now_ms() >= next_scan {
                next_scan = now_ms() + 10_000;
                open = system::processes().iter().any(|(_, n)| NON_REPORTING_PLAYERS.contains(&n.as_str()));
            }
            let audible = if wanted && open {
                let apps = APPS.lock().unwrap().clone();
                apps::audible_player(&apps, NON_REPORTING_PLAYERS)
            } else {
                0
            };
            if let Some(s) = STATE.lock().unwrap().as_mut() {
                s.audible_player = audible;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls the hook as Windows would, without touching the real keyboard.
    fn press(vk: VIRTUAL_KEY, message: u32) -> bool {
        let info = KBDLLHOOKSTRUCT { vkCode: vk.0 as u32, ..Default::default() };
        unsafe { callback(0, WPARAM(message as usize), LPARAM(&info as *const _ as isize)) == LRESULT(1) }
    }

    fn posted() -> Vec<(Key, bool)> {
        std::iter::from_fn(crate::next_event).filter_map(|e| match e { Event::Key(k, down) => Some((k, down)), _ => None }).collect()
    }

    #[test]
    fn media_keys_are_kept_and_reported_once() {
        *STATE.lock().unwrap() = Some(HookState { has_target: true, always_capture: true, ..Default::default() });
        assert!(press(VK_MEDIA_NEXT_TRACK, WM_KEYDOWN));
        assert!(press(VK_MEDIA_NEXT_TRACK, WM_KEYDOWN), "auto-repeat is kept too");
        assert!(press(VK_MEDIA_NEXT_TRACK, WM_KEYUP));
        assert_eq!(posted(), [(Key::Next, true), (Key::Next, false)]);
        assert!(!press(VK_A, WM_KEYDOWN), "other keys pass through");
        // nothing to control and not asked to keep the keys: Windows gets the press and its release
        *STATE.lock().unwrap() = Some(HookState::default());
        assert!(!press(VK_MEDIA_PLAY_PAUSE, WM_KEYDOWN));
        assert!(!press(VK_MEDIA_PLAY_PAUSE, WM_KEYUP));
        assert!(posted().is_empty());
        *STATE.lock().unwrap() = None;
    }

    #[test]
    fn the_hook_installs_and_stops() {
        start();
        assert_ne!(THREAD.load(Relaxed), 0);
        stop();
        assert_eq!(THREAD.load(Relaxed), 0);
    }
}
