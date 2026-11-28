//! Turns a media session's app ID ("Spotify.exe", "Chrome", "308046B0AF4A39CB",
//! "SpotifyAB.SpotifyMusic_…!Spotify") into a friendly name, an icon and the exes its audio comes
//! from; and the per-app volume (the same levels as Windows' Volume Mixer).

use crate::art::Image;
use crate::system::{self, wide};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use windows::Win32::Foundation::{PROPERTYKEY, S_OK, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::*;
use windows_core::{GUID, Interface, PCWSTR, PWSTR};

pub struct AppInfo {
    pub aumid: String,
    pub name: String,
    pub icon: Option<Rc<Image>>,
    /// Lower-case process names without ".exe" that play this app's audio.
    pub exes: Vec<String>,
}

/// App IDs that don't name their exe.
const KNOWN: &[(&str, &str, &str)] = &[
    ("chrome", "Google Chrome", "chrome"),
    ("msedge", "Microsoft Edge", "msedge"),
    ("308046b0af4a39cb", "Firefox", "firefox"),
    ("6f193ccc56814779", "Firefox Nightly", "firefox"),
    ("e7cf176e110c211b", "Firefox", "firefox"),
    ("brave", "Brave", "brave"),
    ("opera", "Opera", "opera"),
    ("vivaldi", "Vivaldi", "vivaldi"),
];

/// Exe names that can be read off the app ID itself.
fn exes_from_id(aumid: &str) -> Vec<String> {
    let id = aumid.to_lowercase();
    let mut exes = Vec::new();
    if let Some(k) = KNOWN.iter().find(|k| k.0 == id) {
        exes.push(k.2.to_string());
    }
    if id.ends_with(".exe") {
        exes.push(system::file_stem_lower(&id));
    }
    // Store apps: in "Publisher.App_hash!AppName", "AppName" is often the process name.
    if let Some((_, app)) = id.split_once('!') {
        exes.push(app.to_string());
    }
    exes.dedup();
    exes
}

pub fn prettify(aumid: &str) -> String {
    let mut s = aumid.split_once('!').map_or(aumid, |(_, a)| a);
    if s.to_lowercase().ends_with(".exe") {
        s = &s[..s.len() - 4];
    }
    if let Some((_, last)) = s.rsplit_once('.').filter(|(_, last)| !last.is_empty()) {
        s = last;
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => aumid.to_string(),
    }
}

thread_local!(static CACHE: RefCell<HashMap<String, Rc<AppInfo>>> = Default::default());

/// Looked up once per app ID. UI thread only (the shell calls need its COM apartment).
pub fn info(aumid: &str) -> Rc<AppInfo> {
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&aumid.to_lowercase()).cloned()) {
        return hit;
    }
    let made = Rc::new(resolve(aumid));
    CACHE.with(|c| c.borrow_mut().insert(aumid.to_lowercase(), made.clone()));
    made
}

fn resolve(aumid: &str) -> AppInfo {
    let mut exes = exes_from_id(aumid);
    let mut name = KNOWN.iter().find(|k| k.0 == aumid.to_lowercase()).map(|k| k.1.to_string());
    // The Start menu entry has the proper name and icon, and for desktop apps the exe path.
    let (shell_name, mut icon, target) = shell_item(&format!(r"shell:AppsFolder\{aumid}"));
    if name.is_none() {
        name = shell_name.filter(|n| !n.trim().is_empty() && !n.contains('\\'));
    }
    if let Some(t) = target.filter(|t| t.to_lowercase().ends_with(".exe")) {
        let stem = system::file_stem_lower(&t);
        if !exes.contains(&stem) {
            exes.push(stem);
        }
    }
    // Fall back to a running process with a matching name.
    if name.is_none() || icon.is_none() {
        let running = system::processes();
        for exe in &exes {
            let Some(path) = running.iter().filter(|(_, n)| n == exe).find_map(|(pid, _)| system::process_identity(*pid).0) else { continue };
            name = name.or_else(|| file_description(&path));
            icon = icon.or_else(|| shell_item(&path).1);
            break;
        }
    }
    AppInfo { aumid: aumid.to_string(), name: name.unwrap_or_else(|| prettify(aumid)), icon: icon.map(Rc::new), exes }
}

/// PKEY_Link_TargetParsingPath
const LINK_TARGET: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25), pid: 2 };

unsafe fn take(p: PWSTR) -> Option<String> {
    unsafe {
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

/// Name, icon and link target of something the shell knows.
fn shell_item(path: &str) -> (Option<String>, Option<Image>, Option<String>) {
    let path = wide(path);
    unsafe {
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem2>(PCWSTR(path.as_ptr()), None) else { return (None, None, None) };
        let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok().and_then(|p| take(p));
        let target = item.GetString(&LINK_TARGET).ok().and_then(|p| take(p));
        let icon = item.cast::<IShellItemImageFactory>().ok().and_then(|f| f.GetImage(SIZE { cx: 64, cy: 64 }, SIIGBF_ICONONLY).ok()).and_then(|b| {
            let img = pixels(b);
            let _ = DeleteObject(b.into());
            img
        });
        (name, icon, target)
    }
}

/// The premultiplied BGRA pixels of a shell bitmap.
unsafe fn pixels(bmp: HBITMAP) -> Option<Image> {
    unsafe {
        let mut info = BITMAP::default();
        if GetObjectW(bmp.into(), size_of::<BITMAP>() as i32, Some(&mut info as *mut _ as _)) == 0 || info.bmWidth <= 0 {
            return None;
        }
        let (w, h) = (info.bmWidth, info.bmHeight.abs());
        let mut img = Image::new(w as u32, h as u32);
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let dc = GetDC(None);
        let lines = GetDIBits(dc, bmp, 0, h as u32, Some(img.px.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == 0 {
            return None;
        }
        if img.px.chunks_exact(4).all(|p| p[3] == 0) {
            img.px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        }
        Some(img)
    }
}

fn file_description(path: &str) -> Option<String> {
    let path = wide(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr() as _).ok()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let q = wide(r"\VarFileInfo\Translation");
        if !VerQueryValueW(data.as_ptr() as _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() || len < 4 {
            return None;
        }
        let t = *(ptr as *const [u16; 2]);
        let q = wide(&format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", t[0], t[1]));
        if !VerQueryValueW(data.as_ptr() as _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr as *const u16, len as usize));
        Some(s.trim_end_matches('\0').trim().to_string()).filter(|s| !s.is_empty())
    }
}

// ---------- audio ----------

pub struct AudioSession {
    pub pid: u32,
    pub exe: String,
    pub aumid: Option<String>,
    volume: ISimpleAudioVolume,
    meter: Option<IAudioMeterInformation>,
}

/// What identifies an app's audio.
#[derive(Clone, Default)]
pub struct AudioId {
    pub aumid: String,
    pub exes: Vec<String>,
}

impl AppInfo {
    pub fn audio_id(&self) -> AudioId {
        AudioId { aumid: self.aumid.clone(), exes: self.exes.clone() }
    }
}

impl AudioSession {
    pub fn belongs(&self, app: &AudioId) -> bool {
        self.aumid.as_deref().is_some_and(|a| crate::config::same_app(a, &app.aumid)) || app.exes.contains(&self.exe)
    }
}

/// Every audio session with a process behind it, on every active output device (system sounds excluded).
pub fn audio_sessions() -> Vec<AudioSession> {
    let mut out = Vec::new();
    let _ = (|| -> windows_core::Result<()> {
        unsafe {
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let list = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut seen: HashMap<u32, (String, Option<String>)> = HashMap::new();
            for d in 0..list.GetCount()? {
                let manager: IAudioSessionManager2 = list.Item(d)?.Activate(CLSCTX_ALL, None)?;
                let sessions = manager.GetSessionEnumerator()?;
                for i in 0..sessions.GetCount()? {
                    let Ok(control) = sessions.GetSession(i).and_then(|s| s.cast::<IAudioSessionControl2>()) else { continue };
                    if control.IsSystemSoundsSession() == S_OK {
                        continue;
                    }
                    let Ok(pid) = control.GetProcessId() else { continue };
                    if pid == 0 {
                        continue;
                    }
                    let (exe, aumid) = seen
                        .entry(pid)
                        .or_insert_with(|| {
                            let (path, aumid) = system::process_identity(pid);
                            (path.map(|p| system::file_stem_lower(&p)).unwrap_or_default(), aumid)
                        })
                        .clone();
                    let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
                    out.push(AudioSession { pid, exe, aumid, volume, meter: control.cast().ok() });
                }
            }
            Ok(())
        }
    })();
    out
}

/// The app's volume (0 to 1) and mute state, or `None` if it has no audio session.
pub fn volume(app: &AudioId) -> Option<(f32, bool)> {
    audio_sessions().iter().filter(|s| s.belongs(app)).find_map(|s| unsafe { Some((s.volume.GetMasterVolume().ok()?, s.volume.GetMute().ok()?.as_bool())) })
}

pub fn set_volume(app: &AudioId, level: f32, mute: Option<bool>) {
    for s in audio_sessions().iter().filter(|s| s.belongs(app)) {
        unsafe {
            let _ = s.volume.SetMasterVolume(level.clamp(0.0, 1.0), std::ptr::null());
            if let Some(m) = mute {
                let _ = s.volume.SetMute(m, std::ptr::null());
            }
        }
    }
}

/// A process making sound that isn't one of the given apps and is one of `players`
/// (a player that doesn't report to Windows' media controls, like VLC 3).
pub fn audible_player(apps: &[AudioId], players: &[&str]) -> u32 {
    audio_sessions()
        .iter()
        .filter(|s| players.contains(&s.exe.as_str()) && !apps.iter().any(|a| s.belongs(a)))
        .find(|s| unsafe { s.meter.as_ref().is_some_and(|m| !s.volume.GetMute().is_ok_and(|m| m.as_bool()) && m.GetPeakValue().is_ok_and(|p| p > 0.0005)) })
        .map_or(0, |s| s.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_exes_from_ids() {
        assert_eq!(exes_from_id("Chrome"), ["chrome"]);
        assert_eq!(exes_from_id("Spotify.exe"), ["spotify"]);
        assert_eq!(exes_from_id("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), ["spotify"]);
        assert_eq!(prettify("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(prettify("com.squirrel.slack.exe"), "Slack");
        assert_eq!(prettify("foobar2000.exe"), "Foobar2000");
    }
}
