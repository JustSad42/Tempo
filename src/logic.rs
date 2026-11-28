//! The rules, free of Windows calls: which app the keys control, where playback is, what the key
//! gestures mean, and whether a key press should be left to Windows.

use crate::config::Settings;

// ---------- which app ----------

/// What choosing a target needs to know about one controllable app.
#[derive(Clone, Debug, Default)]
pub struct Candidate {
    /// Stable identity of the session (not the app ID: an app can have several).
    pub key: u64,
    pub aumid: String,
    pub playing: bool,
    /// When it last started playing, and when it was last seen playing (milliseconds, 0 = never).
    pub started_at: u64,
    pub last_played_at: u64,
}

/// The app the keys control:
///   1. one picked by hand, until some other app starts playing;
///   2. otherwise the one that most recently started playing;
///   3. with nothing playing: the preferred app, else the one that played last, else Windows' own choice.
/// `windows_choice` is asked only if nothing else decides: finding out means calling into Windows.
pub fn target(list: &[Candidate], pinned: Option<(u64, u64)>, settings: &Settings, windows_choice: impl FnOnce() -> Option<String>) -> Option<u64> {
    if let Some((key, at)) = pinned {
        if list.iter().any(|c| c.key == key) && !list.iter().any(|c| c.key != key && c.playing && c.started_at > at) {
            return Some(key);
        }
    }
    // `max_by_key` keeps the last of equals; reverse so the first listed wins a tie.
    if let Some(c) = list.iter().rev().filter(|c| c.playing).max_by_key(|c| c.started_at) {
        return Some(c.key);
    }
    if let Some(c) = list.iter().find(|c| settings.prefers(&c.aumid)) {
        return Some(c.key);
    }
    if let Some(c) = list.iter().rev().filter(|c| c.last_played_at > 0).max_by_key(|c| c.last_played_at) {
        return Some(c.key);
    }
    if list.len() < 2 {
        return list.first().map(|c| c.key);
    }
    let choice = windows_choice();
    list.iter().find(|c| choice.as_deref() == Some(c.aumid.as_str())).or(list.first()).map(|c| c.key)
}

/// The next app after `current`, in the order the pop-up shows them.
pub fn next_target(list: &[Candidate], current: Option<u64>) -> Option<u64> {
    if list.len() < 2 {
        return current;
    }
    let i = list.iter().position(|c| Some(c.key) == current).map_or(0, |i| (i + 1) % list.len());
    Some(list[i].key)
}

// ---------- playback position ----------

/// A position as last reported, moved forward while playing. Times are in seconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timeline {
    pub position: f64,
    pub duration: f64,
    /// Our clock (milliseconds) at the moment `position` was true.
    pub at_ms: u64,
    pub rate: f64,
}

impl Timeline {
    pub fn now(&self, playing: bool, now_ms: u64) -> f64 {
        let mut p = self.position;
        if playing && self.at_ms != 0 {
            let rate = if self.rate > 0.0 { self.rate } else { 1.0 };
            p += now_ms.saturating_sub(self.at_ms) as f64 / 1000.0 * rate;
        }
        if self.duration > 0.0 {
            p = p.min(self.duration);
        }
        p.max(0.0)
    }
}

pub fn format_time(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

// ---------- key gestures ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    PlayPause,
    Next,
    Previous,
    Stop,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Target {
    pub key: u64,
    pub can_seek: bool,
    pub position: f64,
    pub duration: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Do {
    /// Run (or restart) the hold timer.
    Timer(u64),
    StopTimer,
    /// A plain press: send the command, count it, show the pop-up.
    Command(Key, u64),
    /// Nothing to control: just show the pop-up.
    NothingToControl,
    /// Second tap: undo the first one's play/pause and bring the app forward.
    OpenInstead(u64),
    SwitchApp,
    ShowScrub { target: u64, position: f64, from: f64, duration: f64 },
    Seek(u64, f64),
    /// The scrub was released: count it.
    ScrubDone,
}

const DOUBLE_TAP_MS: u64 = 400;

struct Scrub {
    target: Target,
    position: f64,
}

/// tap: the usual; double-tap Play/Pause: open the app; hold Next/Previous: scrub;
/// hold Play/Pause: switch app (keeps switching while held). Holds are timed here, not by key repeat.
#[derive(Default)]
pub struct Gestures {
    held: Option<Key>,
    hold_fired: bool,
    ticks: u32,
    scrub: Option<Scrub>,
    last_live_seek: u64,
    last_play_tap: Option<(u64, u64)>,
}

pub struct Context<'a> {
    pub settings: &'a Settings,
    pub target: Option<Target>,
    pub apps: usize,
    pub now_ms: u64,
}

impl Gestures {
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }

    pub fn down(&mut self, key: Key, cx: &Context) -> Vec<Do> {
        let mut out = Vec::new();
        if let Some(other) = self.held {
            out.extend(self.up(other, cx)); // another key while holding: finish the first
        }
        match key {
            Key::PlayPause if cx.settings.hold_to_switch_app && cx.apps > 1 => self.start_hold(key, 450, &mut out),
            Key::Next | Key::Previous if cx.settings.hold_to_scrub && cx.target.is_some_and(|t| t.can_seek && t.duration > 0.0) => {
                let target = cx.target.unwrap();
                self.scrub = Some(Scrub { target, position: target.position });
                self.start_hold(key, 380, &mut out);
            }
            _ => self.tap(key, cx, &mut out),
        }
        out
    }

    fn start_hold(&mut self, key: Key, ms: u64, out: &mut Vec<Do>) {
        self.held = Some(key);
        self.hold_fired = false;
        self.ticks = 0;
        out.push(Do::Timer(ms));
    }

    pub fn up(&mut self, key: Key, cx: &Context) -> Vec<Do> {
        let mut out = Vec::new();
        if self.held != Some(key) {
            return out;
        }
        self.held = None;
        out.push(Do::StopTimer);
        let scrub = self.scrub.take();
        if !self.hold_fired {
            self.tap(key, cx, &mut out);
        } else if let Some(s) = scrub.filter(|_| matches!(key, Key::Next | Key::Previous)) {
            out.push(Do::Seek(s.target.key, s.position));
            out.push(Do::ScrubDone);
        }
        out
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Do> {
        let Some(key) = self.held else { return vec![Do::StopTimer] };
        self.hold_fired = true;
        self.ticks += 1;
        if key == Key::PlayPause {
            return vec![Do::SwitchApp, Do::Timer(750)];
        }
        let Some(s) = self.scrub.as_mut() else { return vec![Do::StopTimer] };
        // Speeds up the longer the key is held: about 2 s of media per step at first, up to 15 s.
        let step = (2.0 + self.ticks as f64 * 0.25).min(15.0);
        let signed = if key == Key::Next { step } else { -step };
        s.position = (s.position + signed).clamp(0.0, s.target.duration);
        let mut out = vec![Do::Timer(80), Do::ShowScrub { target: s.target.key, position: s.position, from: s.target.position, duration: s.target.duration }];
        // The app follows along while scrubbing, not just at release.
        if now_ms.saturating_sub(self.last_live_seek) >= 350 {
            self.last_live_seek = now_ms;
            out.push(Do::Seek(s.target.key, s.position));
        }
        out
    }

    fn tap(&mut self, key: Key, cx: &Context, out: &mut Vec<Do>) {
        let Some(t) = cx.target else {
            out.push(Do::NothingToControl);
            return;
        };
        if key == Key::PlayPause && cx.settings.double_tap_to_open {
            if self.last_play_tap.is_some_and(|(at, who)| cx.now_ms.saturating_sub(at) < DOUBLE_TAP_MS && who == t.key) {
                self.last_play_tap = None;
                out.push(Do::OpenInstead(t.key));
                return;
            }
            self.last_play_tap = Some((cx.now_ms, t.key));
        }
        out.push(Do::Command(key, t.key));
    }
}

// ---------- leaving keys to Windows ----------

/// Players that don't report to Windows' media controls: while one is making sound, keys go to it.
pub const NON_REPORTING_PLAYERS: &[&str] = &[
    "vlc", "mpc-hc", "mpc-hc64", "mpc-be", "mpc-be64", "potplayer", "potplayer64", "potplayermini", "potplayermini64",
    "winamp", "aimp", "kmplayer", "kmplayer64", "gom", "smplayer", "mpv", "foobar2000", "plex", "plexmediaplayer", "kodi",
];

/// What the keyboard hook knows at the moment of a key press.
#[derive(Clone, Copy, Default, Debug)]
pub struct HookState {
    pub has_target: bool,
    pub target_playing: bool,
    pub always_capture: bool,
    pub pass_through: bool,
    /// Process id of a non-reporting player that is making sound (0 = none).
    pub audible_player: u32,
    /// When a reporting app last started playing (milliseconds).
    pub last_session_start: u64,
}

/// The hook's next state. `target_playing` is `None` when there is nothing to control;
/// `newest_start` is when a playing app last started (kept from before when none is playing).
pub fn hook_state(previous: HookState, target_playing: Option<bool>, newest_start: Option<u64>, settings: &Settings) -> HookState {
    HookState {
        has_target: target_playing.is_some(),
        target_playing: target_playing == Some(true),
        always_capture: settings.always_capture,
        pass_through: settings.pass_through_unknown_players,
        audible_player: previous.audible_player,
        last_session_start: newest_start.unwrap_or(previous.last_session_start),
    }
}

/// Remembers the player the keys were last handed to.
#[derive(Default)]
pub struct PassThrough {
    pid: u32,
    at: u64,
}

impl PassThrough {
    /// Should Tempo take this key press? `Err(true)` means it was handed to another player just now.
    pub fn capture(&mut self, s: &HookState, now_ms: u64, alive: impl Fn(u32) -> bool) -> Result<(), bool> {
        if !s.has_target {
            return if s.always_capture { Ok(()) } else { Err(false) };
        }
        if !s.pass_through || s.target_playing {
            return Ok(());
        }
        if s.audible_player != 0 {
            self.pid = s.audible_player;
            self.at = now_ms;
            return Err(true);
        }
        // Still the last player we gave the keys to (paused now), and no reporting app has started since.
        if self.pid != 0 && s.last_session_start < self.at && alive(self.pid) {
            return Err(false);
        }
        self.pid = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(key: u64, playing: bool, started: u64, last: u64) -> Candidate {
        Candidate { key, aumid: format!("app{key}"), playing, started_at: started, last_played_at: last }
    }

    #[test]
    fn target_rules() {
        let mut s = Settings::default();
        let list = [c(1, true, 10, 50), c(2, true, 20, 40), c(3, false, 5, 90)];
        let unasked = || -> Option<String> { panic!("Windows was asked without need") };
        let says = |a: &'static str| move || Some(a.to_string());
        assert_eq!(target(&list, None, &s, unasked), Some(2), "most recently started playing");
        assert_eq!(target(&list, Some((3, 25)), &s, unasked), Some(3), "picked by hand");
        assert_eq!(target(&list, Some((3, 15)), &s, unasked), Some(2), "another app started after the pick");
        let idle = [c(1, false, 0, 50), c(2, false, 0, 90), c(3, false, 0, 0)];
        assert_eq!(target(&idle, None, &s, unasked), Some(2), "played last");
        s.preferred_app = Some("APP1".into());
        assert_eq!(target(&idle, None, &s, unasked), Some(1), "preferred beats played last");
        s.preferred_app = None;
        let never = [c(1, false, 0, 0), c(2, false, 0, 0)];
        assert_eq!(target(&never, None, &s, says("app2")), Some(2), "Windows' choice");
        assert_eq!(target(&never, None, &s, || None), Some(1));
        assert_eq!(target(&[], None, &s, unasked), None);
        assert_eq!(next_target(&never, Some(2)), Some(1));
        assert_eq!(next_target(&never[..1], Some(1)), Some(1));
    }

    #[test]
    fn position_moves_only_while_playing() {
        let t = Timeline { position: 10.0, duration: 20.0, at_ms: 1000, rate: 2.0 };
        assert_eq!(t.now(false, 9000), 10.0);
        assert_eq!(t.now(true, 3000), 14.0);
        assert_eq!(t.now(true, 99_000), 20.0);
        assert_eq!(format_time(65.9), "1:05");
        assert_eq!(format_time(3725.0), "1:02:05");
    }

    const T: Target = Target { key: 7, can_seek: true, position: 100.0, duration: 200.0 };

    fn cx(s: &Settings, apps: usize, now: u64) -> Context<'_> {
        Context { settings: s, target: Some(T), apps, now_ms: now }
    }

    #[test]
    fn taps_and_double_tap() {
        let s = Settings::default();
        let mut g = Gestures::default();
        assert_eq!(g.down(Key::PlayPause, &cx(&s, 1, 1000)), [Do::Command(Key::PlayPause, 7)]);
        assert_eq!(g.down(Key::PlayPause, &cx(&s, 1, 1300)), [Do::OpenInstead(7)]);
        assert_eq!(g.down(Key::PlayPause, &cx(&s, 1, 1400)), [Do::Command(Key::PlayPause, 7)], "a third tap starts over");
        assert_eq!(g.down(Key::PlayPause, &cx(&s, 1, 2000)), [Do::Command(Key::PlayPause, 7)], "too slow");
        assert_eq!(g.down(Key::Stop, &cx(&s, 1, 3000)), [Do::Command(Key::Stop, 7)]);
        let nothing = Context { settings: &s, target: None, apps: 0, now_ms: 0 };
        assert_eq!(g.down(Key::Next, &nothing), [Do::NothingToControl]);
    }

    #[test]
    fn holds() {
        let s = Settings::default();
        let mut g = Gestures::default();
        // short press with a hold available: acts on release
        assert_eq!(g.down(Key::Next, &cx(&s, 1, 0)), [Do::Timer(380)]);
        assert_eq!(g.up(Key::Next, &cx(&s, 1, 100)), [Do::StopTimer, Do::Command(Key::Next, 7)]);
        // held: scrubs, follows along, seeks on release
        g.down(Key::Previous, &cx(&s, 1, 1000));
        let a = g.tick(1400);
        assert_eq!(a, [Do::Timer(80), Do::ShowScrub { target: 7, position: 97.75, from: 100.0, duration: 200.0 }, Do::Seek(7, 97.75)]);
        assert_eq!(g.tick(1480).len(), 2, "no live seek within 350 ms");
        let up = g.up(Key::Previous, &cx(&s, 1, 1500));
        assert_eq!(up, [Do::StopTimer, Do::Seek(7, 95.25), Do::ScrubDone]);
        // play/pause held with two apps: switches, repeatedly; releasing does nothing more
        assert_eq!(g.down(Key::PlayPause, &cx(&s, 2, 5000)), [Do::Timer(450)]);
        assert_eq!(g.tick(5450), [Do::SwitchApp, Do::Timer(750)]);
        assert_eq!(g.up(Key::PlayPause, &cx(&s, 2, 5600)), [Do::StopTimer]);
        assert!(!g.holding());
        // a second key finishes the first
        g.down(Key::Next, &cx(&s, 1, 6000));
        let both = g.down(Key::Stop, &cx(&s, 1, 6050));
        assert_eq!(both, [Do::StopTimer, Do::Command(Key::Next, 7), Do::Command(Key::Stop, 7)]);
    }

    #[test]
    fn hook_state_remembers_the_last_start() {
        let s = Settings::default();
        let first = hook_state(HookState { audible_player: 9, ..Default::default() }, Some(true), Some(100), &s);
        assert!(first.has_target && first.target_playing && first.always_capture);
        let later = hook_state(first, None, None, &s);
        assert_eq!((later.has_target, later.audible_player, later.last_session_start), (false, 9, 100));
    }

    #[test]
    fn keys_go_to_players_windows_cannot_see() {
        let base = HookState { has_target: true, pass_through: true, always_capture: true, ..Default::default() };
        let mut p = PassThrough::default();
        assert_eq!(p.capture(&HookState { has_target: false, ..base }, 1, |_| true), Ok(()));
        assert_eq!(p.capture(&HookState { has_target: false, always_capture: false, ..base }, 1, |_| true), Err(false));
        assert_eq!(p.capture(&HookState { audible_player: 42, ..base }, 100, |_| true), Err(true));
        assert_eq!(p.capture(&base, 200, |_| true), Err(false), "paused player keeps the keys");
        assert_eq!(p.capture(&HookState { target_playing: true, ..base }, 300, |_| true), Ok(()));
        assert_eq!(p.capture(&HookState { last_session_start: 150, ..base }, 400, |_| true), Ok(()), "another app started since");
        p.capture(&HookState { audible_player: 42, ..base }, 500, |_| true).unwrap_err();
        assert_eq!(p.capture(&base, 600, |_| false), Ok(()), "player closed");
    }
}
