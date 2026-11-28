//! Settings, history and stats in %APPDATA%\Tempo. The JSON is what Tempo 1.x (C#) wrote, so an
//! update keeps everything.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Placement {
    TopLeft,
    #[default]
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Placement {
    pub fn is_top(self) -> bool {
        matches!(self, Placement::TopLeft | Placement::TopCenter | Placement::TopRight)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    pub placement: Placement,
    /// Show on the screen under the mouse (otherwise the primary screen).
    pub follow_mouse: bool,
    pub popup_seconds: f64,
    pub show_popup: bool,
    pub hide_in_fullscreen: bool,
    pub hold_to_scrub: bool,
    pub hold_to_switch_app: bool,
    pub skip_seconds: u32,
    /// Let the key through to Windows when the only thing making sound is a player Tempo can't control.
    pub pass_through_unknown_players: bool,
    pub welcome_shown: bool,
    /// App ID that gets the keys when nothing is playing.
    pub preferred_app: Option<String>,
    pub double_tap_to_open: bool,
    /// Keep the keys from Windows even when no media app is open.
    pub always_capture: bool,
    pub auto_update: bool,
    /// App IDs the media keys never control.
    pub ignored_apps: Vec<String>,
    /// Every app seen so far (app ID to name), for the settings list.
    pub known_apps: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            placement: Placement::TopCenter,
            follow_mouse: true,
            popup_seconds: 2.5,
            show_popup: true,
            hide_in_fullscreen: false,
            hold_to_scrub: true,
            hold_to_switch_app: true,
            skip_seconds: 10,
            pass_through_unknown_players: true,
            welcome_shown: false,
            preferred_app: None,
            double_tap_to_open: true,
            always_capture: true,
            auto_update: true,
            ignored_apps: Vec::new(),
            known_apps: BTreeMap::new(),
        }
    }
}

/// App IDs compare without case.
pub fn same_app(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl Settings {
    pub fn ignores(&self, id: &str) -> bool {
        self.ignored_apps.iter().any(|a| same_app(a, id))
    }

    pub fn set_ignored(&mut self, id: &str, ignored: bool) {
        self.ignored_apps.retain(|a| !same_app(a, id));
        if ignored {
            self.ignored_apps.push(id.to_string());
        }
    }

    pub fn prefers(&self, id: &str) -> bool {
        self.preferred_app.as_deref().is_some_and(|p| same_app(p, id))
    }

    /// Records an app's name. True when something changed.
    pub fn remember(&mut self, id: &str, name: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        let key = self.known_apps.keys().find(|k| same_app(k, id)).cloned().unwrap_or_else(|| id.to_string());
        if self.known_apps.get(&key).is_some_and(|n| n == name) {
            return false;
        }
        self.known_apps.insert(key, name.to_string());
        true
    }
}

/// Accepts the time forms .NET writes: with an offset, with `Z`, or with neither (local time).
fn de_time<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Local>, D::Error> {
    let s = String::deserialize(d)?;
    parse_time(&s).ok_or_else(|| serde::de::Error::custom("not a time"))
}

pub fn parse_time(s: &str) -> Option<DateTime<Local>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Local));
    }
    let n = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    Local.from_local_datetime(&n).earliest()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HistoryItem {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub aumid: String,
    #[serde(deserialize_with = "de_time")]
    pub at: DateTime<Local>,
}

pub const HISTORY_MAX: usize = 50;

/// Adds a played track, newest first. The same track within half an hour is not a new entry.
pub fn add_history(items: &mut Vec<HistoryItem>, new: HistoryItem) -> bool {
    if new.title.trim().is_empty() {
        return false;
    }
    if items.iter().any(|h| h.title == new.title && h.artist == new.artist && (new.at - h.at).num_minutes() < 30) {
        return false;
    }
    items.insert(0, new);
    items.truncate(HISTORY_MAX);
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Stats {
    #[serde(deserialize_with = "de_time")]
    pub since: DateTime<Local>,
    pub keys_handled: u64,
    pub keys_today: u64,
    #[serde(deserialize_with = "de_time")]
    pub today: DateTime<Local>,
    /// Key presses that went to a different app than Windows would have chosen.
    pub better_than_windows: u64,
    pub passed_to_other_players: u64,
    pub app_switches: u64,
    pub scrubs: u64,
    pub hook_recoveries: u64,
}

impl Default for Stats {
    fn default() -> Self {
        let now = Local::now();
        Stats { since: now, keys_handled: 0, keys_today: 0, today: midnight(now), better_than_windows: 0, passed_to_other_players: 0, app_switches: 0, scrubs: 0, hook_recoveries: 0 }
    }
}

fn midnight(t: DateTime<Local>) -> DateTime<Local> {
    t.date_naive().and_hms_opt(0, 0, 0).and_then(|n| Local.from_local_datetime(&n).earliest()).unwrap_or(t)
}

impl Stats {
    pub fn key(&mut self, better_than_windows: bool, now: DateTime<Local>) {
        if self.today.date_naive() != now.date_naive() {
            self.today = midnight(now);
            self.keys_today = 0;
        }
        self.keys_handled += 1;
        self.keys_today += 1;
        if better_than_windows {
            self.better_than_windows += 1;
        }
    }
}

pub fn instance_tag() -> String {
    std::env::var("TEMPO_INSTANCE").ok().filter(|t| !t.is_empty()).map(|t| format!(".{t}")).unwrap_or_default()
}

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TEMPO_DATA_DIR").filter(|d| !d.is_empty()) {
        return d.into();
    }
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_else(|| ".".into())).join("Tempo")
}

pub fn parse<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
}

/// Reads a file, or `None` if it's missing or unreadable. An unreadable one is copied to
/// `<name>.bad` first, because the defaults used instead will be saved over it.
pub fn load<T: DeserializeOwned>(name: &str) -> Option<T> {
    let path = dir().join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    match parse(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            let _ = std::fs::copy(&path, dir().join(format!("{name}.bad")));
            log(&format!("Couldn't read {name} ({e}); kept a copy as {name}.bad."));
            None
        }
    }
}

/// Writes beside the file and renames over it, so a crash never leaves half a file.
pub fn save<T: Serialize>(name: &str, value: &T) {
    let Ok(text) = serde_json::to_string_pretty(value) else { return };
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!("{name}.tmp"));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(name));
    }
}

pub fn log(message: &str) {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("log.txt");
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = std::fs::remove_file(&file);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{}  {message}\r", Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_tempo_1_wrote() {
        let s: Settings = serde_json::from_str(
            r#"{"Placement":"BottomRight","FollowMouse":false,"PopupSeconds":4,"ShowPopup":true,"SkipSeconds":15,
                "PreferredApp":null,"IgnoredApps":["Chrome"],"KnownApps":{"Spotify.exe":"Spotify"},"Unknown":1}"#,
        )
        .unwrap();
        assert_eq!((s.placement, s.follow_mouse, s.popup_seconds, s.skip_seconds), (Placement::BottomRight, false, 4.0, 15));
        assert!(s.ignores("chrome") && s.double_tap_to_open);
        assert_eq!(s.known_apps["Spotify.exe"], "Spotify");
        // .NET writes seven decimals and an offset
        let h: Vec<HistoryItem> = serde_json::from_str(r#"[{"Title":"T","Artist":"A","App":"S","Aumid":"x","At":"2026-11-20T21:04:05.1234567+02:00"}]"#).unwrap();
        assert_eq!(h[0].at.to_utc().to_rfc3339().get(..19), Some("2026-11-20T19:04:05"));
        let st: Stats = serde_json::from_str(r#"{"Since":"2026-11-01T10:00:00.5+02:00","KeysHandled":7,"KeysToday":2,"Today":"2026-11-20T00:00:00+02:00"}"#).unwrap();
        assert_eq!((st.keys_handled, st.scrubs), (7, 0));
        // and what we write reads back
        let again: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(again.placement, s.placement);
        assert!(serde_json::to_string(&s).unwrap().contains("\"PopupSeconds\""));
    }

    #[test]
    fn history_skips_repeats_and_stays_short() {
        let at = Local::now();
        let item = |t: &str, m: i64| HistoryItem { title: t.into(), artist: "a".into(), app: "".into(), aumid: "".into(), at: at + chrono::Duration::minutes(m) };
        let mut v = Vec::new();
        assert!(add_history(&mut v, item("one", 0)));
        assert!(!add_history(&mut v, item("one", 5)));
        assert!(add_history(&mut v, item("one", 31)));
        assert!(!add_history(&mut v, item("  ", 40)));
        for i in 0..60 {
            add_history(&mut v, item(&format!("t{i}"), 50));
        }
        assert_eq!(v.len(), HISTORY_MAX);
        assert_eq!(v[0].title, "t59");
    }

    /// `cargo test this_pcs_files -- --ignored`: the real files from an installed Tempo parse.
    /// Reads only, and prints counts, never contents.
    #[test]
    #[ignore]
    fn this_pcs_files() {
        let read = |name: &str| std::fs::read_to_string(dir().join(name)).ok();
        if let Some(t) = read("settings.json") {
            let s: Settings = parse(&t).expect("settings.json");
            println!("settings: ok, {} known apps, {} ignored", s.known_apps.len(), s.ignored_apps.len());
        }
        if let Some(t) = read("history.json") {
            let h: Vec<HistoryItem> = parse(&t).expect("history.json");
            println!("history: ok, {} tracks", h.len());
        }
        if let Some(t) = read("stats.json") {
            let s: Stats = parse(&t).expect("stats.json");
            println!("stats: ok, {} key presses", s.keys_handled);
        }
    }

    #[test]
    fn todays_count_restarts() {
        let mut s = Stats::default();
        let now = Local::now();
        s.key(true, now);
        s.key(false, now);
        s.key(false, now + chrono::Duration::days(1));
        assert_eq!((s.keys_handled, s.keys_today, s.better_than_windows), (3, 1, 1));
    }
}
