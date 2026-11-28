//! Small Windows services: processes, windows, monitors, clipboard, start-up entry, shortcut.

use std::path::{Path, PathBuf};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_MENU, keybd_event};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{BOOL, Interface, PCWSTR, PWSTR, w};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

pub fn file_stem_lower(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

// ---------- processes ----------

/// (process id, exe name without ".exe", lower case) of every running process.
pub fn processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, file_stem_lower(&String::from_utf16_lossy(&e.szExeFile[..len]))));
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Exe path and (for Store apps) the app ID of a process. Either may be missing.
pub fn process_identity(pid: u32) -> (Option<String>, Option<String>) {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return (None, None) };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let path = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len).is_ok().then(|| String::from_utf16_lossy(&buf[..len as usize]));
        let mut len = buf.len() as u32;
        let aumid = (GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr()))) == ERROR_SUCCESS && len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize - 1]));
        let _ = CloseHandle(h);
        (path, aumid)
    }
}

pub fn process_alive(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0;
        let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(h);
        alive
    }
}

pub fn wait_for_exit(pid: u32, ms: u32) {
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, ms);
            let _ = CloseHandle(h);
        }
    }
}

// ---------- an app's window ----------

struct Search<'a> {
    exes: &'a [String],
    aumid: &'a str,
    title: String,
    best: Option<(u32, HWND)>,
    seen: std::collections::HashMap<u32, bool>,
}

unsafe extern "system" fn visit(h: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let s = &mut *(lp.0 as *mut Search);
        if !IsWindowVisible(h).as_bool() || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return true.into();
        }
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let matches = *s.seen.entry(pid).or_insert_with(|| {
            let (path, aumid) = process_identity(pid);
            path.is_some_and(|p| s.exes.contains(&file_stem_lower(&p))) || aumid.is_some_and(|a| crate::config::same_app(&a, s.aumid))
        });
        if !matches {
            return true.into();
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(h, &mut buf);
        if n <= 0 {
            return true.into();
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]).to_lowercase();
        let score = if !s.title.trim().is_empty() && title.contains(&s.title) { 3 } else { 1 };
        if s.best.is_none_or(|(b, _)| score > b) {
            s.best = Some((score, h));
        }
        true.into()
    }
}

/// Brings forward a window of the app, preferring one whose title names the track (a browser
/// window showing the playing tab). False if there is none.
pub fn focus_app_window(exes: &[String], aumid: &str, title: &str) -> bool {
    let mut s = Search { exes, aumid, title: title.to_lowercase(), best: None, seen: Default::default() };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut s as *mut _ as isize));
        let Some((_, h)) = s.best else { return false };
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        // Windows only lets the app with the latest input take the foreground; a tap of Alt counts as that.
        keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        let _ = SetForegroundWindow(h);
        GetForegroundWindow() == h
    }
}

// ---------- screens ----------

/// Work area (physical pixels) and scale of the monitor under the mouse, or of the primary one.
pub fn work_area(under_mouse: bool) -> (RECT, f32) {
    unsafe {
        let mut pt = POINT::default();
        if under_mouse {
            let _ = GetCursorPos(&mut pt);
        }
        let mon = MonitorFromPoint(pt, if under_mouse { MONITOR_DEFAULTTONEAREST } else { MONITOR_DEFAULTTOPRIMARY });
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut x, mut y) = (96, 96);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        (info.rcWork, x as f32 / 96.0)
    }
}

/// True when a fullscreen game, video or presentation owns the screen.
pub fn fullscreen_active() -> bool {
    unsafe { SHQueryUserNotificationState().is_ok_and(|s| s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE) }
}

// ---------- clipboard, browser ----------

const CF_UNICODETEXT: u32 = 13;

pub fn copy_text(owner: HWND, text: &str) -> bool {
    let data = wide(text);
    for _ in 0..5 {
        unsafe {
            if OpenClipboard(Some(owner)).is_ok() {
                let ok = (|| -> Option<()> {
                    EmptyClipboard().ok()?;
                    let mem = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2).ok()?;
                    let to = GlobalLock(mem) as *mut u16;
                    if to.is_null() {
                        return None;
                    }
                    std::ptr::copy_nonoverlapping(data.as_ptr(), to, data.len());
                    let _ = GlobalUnlock(mem);
                    SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).ok()?;
                    Some(())
                })()
                .is_some();
                let _ = CloseClipboard();
                if ok {
                    return true;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    false
}

pub fn open_url(url: &str) {
    let url = wide(url);
    unsafe { ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
}

pub fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

// ---------- start with Windows ----------

const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const NAME: PCWSTR = w!("Tempo");

pub fn autostart_value() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size)).ok().ok()? };
    Some(String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}

pub fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyW(HKEY_CURRENT_USER, RUN, &mut key).is_err() {
            return;
        }
        if on {
            let value = wide(&format!("\"{}\" --background", exe_path().display()));
            let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
            let _ = RegSetValueExW(key, NAME, None, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, NAME);
        }
        let _ = RegCloseKey(key);
    }
}

/// Keeps the entry pointing at this exe if the app was moved.
pub fn repair_autostart() {
    let here = exe_path().to_string_lossy().to_lowercase();
    if autostart_value().is_some_and(|v| !v.to_lowercase().contains(&here)) {
        set_autostart(true);
    }
}

/// Start menu entry, so Tempo (and its settings) can be found without a tray icon.
pub fn ensure_start_menu_shortcut() {
    let exe = exe_path();
    if exe.to_string_lossy().to_lowercase().contains(r"\target\") {
        return;
    }
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let link = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Tempo.lnk");
    let result = (|| -> windows_core::Result<()> {
        unsafe {
            let shortcut: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let file: IPersistFile = shortcut.cast()?;
            let link_w = wide(&link.to_string_lossy());
            if link.exists() && file.Load(PCWSTR(link_w.as_ptr()), STGM_READ).is_ok() {
                let mut target = [0u16; 1024];
                shortcut.GetPath(&mut target, std::ptr::null_mut(), 0)?;
                let len = target.iter().position(|c| *c == 0).unwrap_or(0);
                if String::from_utf16_lossy(&target[..len]).to_lowercase() == exe.to_string_lossy().to_lowercase() {
                    return Ok(());
                }
            }
            let path = wide(&exe.to_string_lossy());
            let dir = wide(&exe.parent().unwrap_or(Path::new("")).to_string_lossy());
            shortcut.SetPath(PCWSTR(path.as_ptr()))?;
            shortcut.SetWorkingDirectory(PCWSTR(dir.as_ptr()))?;
            shortcut.SetDescription(w!("Tempo media keys - opens settings"))?;
            file.Save(PCWSTR(link_w.as_ptr()), true)
        }
    })();
    if let Err(e) = result {
        crate::config::log(&format!("Start menu shortcut failed: {e}"));
    }
}
