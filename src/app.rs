//! The running app: owns the state and turns events into actions.

use crate::config::{self, HistoryItem, Settings, Stats};
use crate::hook;
use crate::logic::{self, Do, Gestures, Key};
use crate::media::Hub;
use crate::popup::{self, Act, Flash, Popup};
use crate::settings_ui::{self as ui, Action, Toggle};
use crate::system::{self, now_ms, wide};
use crate::{Event, HOST, Signal, apps, art, post, supervisor, updater};
use chrono::{DateTime, Local};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::PCWSTR;

const TIMER_HOLD: usize = 1;
const TIMER_REHOOK: usize = 2;
const TIMER_FLUSH: usize = 3;
const TIMER_UPDATE_CHECK: usize = 4;
const TIMER_UPDATE_APPLY: usize = 5;
const TIMER_RAW_CHECK: usize = 6;
const TIMER_RESYNC_SOON: usize = 7;
const TIMER_RESYNC_LATER: usize = 8;

struct App {
    host: HWND,
    settings: Settings,
    stats: Stats,
    stats_dirty: bool,
    history: Vec<HistoryItem>,
    hub: Hub,
    gestures: Gestures,
    popup: Popup,
    window: Option<ui::Window>,
    /// App ids behind the rows of the settings list, in order.
    app_rows: Vec<String>,
    logo: Rc<art::Image>,
    snooze_until: u64,
    /// The last volume read: (session, when, value). Reading it lists every audio session.
    volume: std::cell::Cell<(u64, u64, Option<(f32, bool)>)>,
    /// The app whose command should be read back (apps can be slow to report the new state).
    resync: Option<u64>,
    /// When raw input saw a media key that the hook should have seen too.
    raw_seen: u64,
    last_recovery: u64,
    test: bool,
    quit: bool,
}

thread_local!(static APP: RefCell<Option<App>> = const { RefCell::new(None) });

pub fn pump() {
    APP.with(|cell| {
        // Already borrowed: the loop that holds it will pick the event up.
        let Ok(mut slot) = cell.try_borrow_mut() else { return };
        let Some(app) = slot.as_mut() else { return };
        while let Some(event) = crate::next_event() {
            app.handle(event);
        }
        if app.quit {
            unsafe { PostQuitMessage(0) };
        }
    });
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => post(Event::Timer(wp.0)),
            WM_INPUT => {
                if hook::is_media_key_down(lp, !config::instance_tag().is_empty()) {
                    post(Event::RawMediaKey);
                }
            }
            WM_POWERBROADCAST if wp.0 as u32 == PBT_APMRESUMEAUTOMATIC => post(Event::Resumed),
            // Signing out: save, and exit with success so the supervisor lets go.
            WM_ENDSESSION if wp.0 != 0 => {
                APP.with(|c| {
                    if let Some(app) = c.try_borrow_mut().ok().as_mut().and_then(|s| s.as_mut()) {
                        app.flush_stats();
                    }
                });
                std::process::exit(0);
            }
            _ => return DefWindowProcW(h, msg, wp, lp),
        }
        // WM_INPUT must still reach the default handler so the system can clean up.
        if msg == WM_INPUT { DefWindowProcW(h, msg, wp, lp) } else { LRESULT(1) }
    }
}

/// A hidden top-level window (message-only ones don't get power broadcasts).
fn host_window() -> HWND {
    let class = wide("TempoHost");
    unsafe {
        let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: PCWSTR(class.as_ptr()), hInstance: module.unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(WS_EX_TOOLWINDOW, PCWSTR(class.as_ptr()), PCWSTR(class.as_ptr()), WS_POPUP, 0, 0, 0, 0, None, None, module, None).unwrap_or_default()
    }
}

/// Posts a signal each time another process sets the named event.
fn listen(object: &str, make: fn() -> Signal) {
    let n = supervisor::name(object);
    let Ok(event) = (unsafe { CreateEventW(None, false, false, PCWSTR(n.as_ptr())) }) else { return };
    let raw = event.0 as isize;
    std::thread::spawn(move || {
        while unsafe { WaitForSingleObject(HANDLE(raw as _), INFINITE) } == WAIT_OBJECT_0 {
            post(Event::Signal(make()));
        }
    });
}

fn app_icon() -> Rc<art::Image> {
    // The 64-pixel logo, kept as raw premultiplied BGRA so no image decoder is needed.
    Rc::new(art::Image { w: 64, h: 64, px: include_bytes!("../assets/logo-64.bgra").to_vec() })
}

pub fn run(args: &[String]) {
    updater::clean_up();
    let Some(_instance) = supervisor::lock("SingleInstance", 0) else {
        supervisor::signal(if args.iter().any(|a| a == "--preview") { "Preview" } else { "ShowSettings" });
        return;
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let has = |f: &str| args.iter().any(|a| a == f);
    let test = !config::instance_tag().is_empty();
    // Test only: crash on purpose to check that the supervisor brings Tempo back.
    if test && !has("--restarted") {
        if let Some(after) = std::env::var("TEMPO_TEST_CRASH_AFTER").ok().and_then(|v| v.parse::<u64>().ok()) {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(after));
                std::process::abort();
            });
        }
    }

    let first_run = !config::dir().join("settings.json").exists();
    let settings: Settings = config::load("settings.json").unwrap_or_default();
    if first_run {
        config::save("settings.json", &settings);
        if !test {
            system::set_autostart(true);
        }
    } else if !test {
        system::repair_autostart();
    }

    let host = host_window();
    HOST.store(host.0 as isize, SeqCst);
    let app = App {
        host,
        settings,
        stats: config::load("stats.json").unwrap_or_default(),
        stats_dirty: false,
        history: config::load("history.json").unwrap_or_default(),
        hub: Hub::default(),
        gestures: Gestures::default(),
        popup: Popup::new(),
        window: None,
        app_rows: Vec::new(),
        logo: app_icon(),
        snooze_until: 0,
        volume: Default::default(),
        resync: None,
        raw_seen: 0,
        last_recovery: 0,
        test,
        quit: false,
    };
    app.publish();
    crate::media::request_manager();
    listen("ShowSettings", || Signal::ShowSettings);
    listen("Preview", || Signal::Preview);
    listen("TestDropHook", || Signal::DropHook);
    app.start_hook();
    hook::watch_raw_input(host);
    hook::start_audio_watch();
    // Windows silently drops hooks that ever answer too slowly: renew it now and then.
    app.timer(TIMER_REHOOK, 30 * 60 * 1000);
    app.timer(TIMER_FLUSH, 30_000);
    app.timer(TIMER_UPDATE_CHECK, 30_000);
    if has("--updated") {
        config::log(&format!("Updated to {}.", updater::VERSION));
    }
    if !test {
        system::ensure_start_menu_shortcut();
    }
    if has("--preview") {
        post(Event::Signal(Signal::Preview));
    } else if !has("--background") {
        post(Event::Signal(Signal::ShowSettings));
    }
    config::log(&format!("Started {}{}{}.", updater::VERSION, if first_run { " (first run)" } else { "" }, if has("--restarted") { " (restarted after a crash)" } else { "" }));

    APP.with(|c| *c.borrow_mut() = Some(app));
    pump();
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            pump();
        }
    }
    APP.with(|c| {
        if let Some(app) = c.borrow_mut().as_mut() {
            app.flush_stats();
        }
    });
    hook::stop();
}

impl App {
    fn timer(&self, id: usize, ms: u32) {
        unsafe { SetTimer(Some(self.host), id, ms, None) };
    }

    fn stop_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.host), id);
        }
    }

    fn start_hook(&self) {
        // Test only: look and measure without touching the keyboard.
        if self.test && std::env::var("TEMPO_NO_HOOK").is_ok_and(|v| v == "1") {
            return;
        }
        hook::start();
    }

    fn rehook(&self) {
        hook::stop();
        self.start_hook();
    }

    fn flush_stats(&mut self) {
        let passed = hook::PASSED.swap(0, Relaxed) as u64;
        self.stats.passed_to_other_players += passed;
        if self.stats_dirty || passed > 0 {
            self.stats_dirty = false;
            config::save("stats.json", &self.stats);
        }
    }

    fn count(&mut self, change: impl FnOnce(&mut Stats)) {
        change(&mut self.stats);
        self.stats_dirty = true;
        self.refresh_window();
    }

    fn save_settings(&mut self) {
        config::save("settings.json", &self.settings);
        self.changed();
    }

    /// Tells the keyboard hook and the audio watch what they need to know.
    fn publish(&self) {
        let target = self.hub.target(&self.settings).and_then(|k| self.hub.get(k));
        // Read once, in its own statement: the lock isn't re-entrant.
        let previous = hook::STATE.lock().unwrap().unwrap_or_default();
        let newest_start = self.hub.sessions.iter().filter(|s| s.playing).map(|s| s.started_at).max();
        let state = logic::hook_state(previous, target.map(|t| t.playing), newest_start, &self.settings);
        *hook::APPS.lock().unwrap() = self.hub.controllable(&self.settings).map(|s| s.app.audio_id()).collect();
        *hook::STATE.lock().unwrap() = Some(state);
    }

    /// Sessions, their state or the settings changed.
    fn changed(&mut self) {
        let mut remembered = false;
        for s in &self.hub.sessions {
            remembered |= self.settings.remember(&s.app.aumid, &s.app.name);
        }
        if remembered {
            config::save("settings.json", &self.settings);
        }
        for (title, artist, app) in self.hub.take_started() {
            let item = HistoryItem { title, artist, app: app.name.clone(), aumid: app.aumid.clone(), at: Local::now() };
            if config::add_history(&mut self.history, item) {
                config::save("history.json", &self.history);
            }
        }
        self.publish();
        if self.popup.visible() {
            let view = self.view();
            self.popup.set_view(view);
        }
        self.refresh_window();
    }

    // ---------- the pop-up ----------

    fn view(&self) -> popup::View {
        let target = self.hub.target(&self.settings).and_then(|k| self.hub.get(k));
        popup::View {
            target: target.map(|s| popup::Track {
                key: s.key,
                app: s.app.name.clone(),
                icon: s.app.icon.clone(),
                title: s.title.clone(),
                artist: s.artist.clone(),
                art: s.art.clone(),
                accent: s.accent,
                playing: s.playing,
                can_previous: s.can_previous,
                can_next: s.can_next,
                can_seek: s.can_seek,
                timeline: s.timeline,
            }),
            sources: self.hub.controllable(&self.settings).map(|s| popup::Source { key: s.key, name: s.app.name.clone(), title: s.title.clone(), icon: s.app.icon.clone(), playing: s.playing }).collect(),
            volume: target.and_then(|s| self.volume_of(s)),
            skip: self.settings.skip_seconds,
            preferred: target.is_some_and(|s| self.settings.prefers(&s.app.aumid)),
        }
    }

    /// The app's volume, read at most once a second while the pop-up keeps redrawing.
    fn volume_of(&self, session: &crate::media::Session) -> Option<(f32, bool)> {
        let (key, at, value) = self.volume.get();
        if key == session.key && now_ms() < at + 1000 {
            return value;
        }
        let value = apps::volume(&session.app.audio_id());
        self.volume.set((session.key, now_ms(), value));
        value
    }

    fn popup_allowed(&self) -> bool {
        self.settings.show_popup && now_ms() >= self.snooze_until && !(self.settings.hide_in_fullscreen && system::fullscreen_active())
    }

    fn show_popup(&mut self) {
        let view = self.view();
        self.popup.show(view, self.settings.placement, self.settings.follow_mouse, self.settings.popup_seconds);
    }

    /// Shows the pop-up for a key press or click and pulses the matching control.
    fn flash(&mut self, what: Flash, from_mouse: bool) {
        if !from_mouse && !self.popup_allowed() {
            return;
        }
        self.show_popup();
        self.popup.flash(what);
        self.popup.restart_hide(self.settings.popup_seconds);
    }

    fn notice(&mut self, text: &str) {
        self.popup.notice(text);
    }

    /// Brings the app's window forward (the browser window showing the playing tab, if it can tell).
    fn open_app(&mut self, key: u64) {
        let Some(s) = self.hub.get(key) else { return };
        let (app, title) = (s.app.clone(), s.title.clone());
        let ok = system::focus_app_window(&app.exes, &app.aumid, &title);
        if self.popup.visible() || self.popup_allowed() {
            self.show_popup();
            self.notice(&if ok { format!("Opened {}", app.name) } else { format!("{} has no window", app.name) });
            self.popup.restart_hide(self.settings.popup_seconds.min(1.6));
        }
    }

    fn copy_song(&mut self, title: &str, artist: &str) -> bool {
        let text = if artist.trim().is_empty() { title.to_string() } else { format!("{artist} \u{2013} {title}") };
        system::copy_text(self.host, &text)
    }

    /// Reads the app's state back shortly after a command.
    fn resync(&mut self, key: u64) {
        self.resync = Some(key);
        self.timer(TIMER_RESYNC_SOON, 350);
        self.timer(TIMER_RESYNC_LATER, 1200);
    }

    fn command(&mut self, key: Key, target: u64) {
        match key {
            Key::PlayPause => self.hub.play_pause(target),
            Key::Next => self.hub.next(target),
            Key::Previous => self.hub.previous(target),
            Key::Stop => self.hub.stop(target),
        }
        self.resync(target);
        self.changed();
    }

    fn popup_act(&mut self, act: Act) {
        let target = self.popup.target_key();
        match (act, target) {
            (Act::PlayPause, Some(t)) => {
                self.command(Key::PlayPause, t);
                self.flash(Flash::PlayPause, true);
            }
            (Act::Next, Some(t)) => {
                self.command(Key::Next, t);
                self.flash(Flash::Next, true);
            }
            (Act::Previous, Some(t)) => {
                self.command(Key::Previous, t);
                self.flash(Flash::Previous, true);
            }
            (Act::Skip(by), Some(t)) => {
                if let Some(p) = self.hub.get(t).map(|s| s.position()) {
                    self.hub.seek(t, p + by);
                    self.changed();
                }
            }
            (Act::Seek(p), Some(t)) => {
                self.hub.seek(t, p);
                self.changed();
            }
            (Act::Volume(v), Some(t)) => {
                if let Some(s) = self.hub.get(t) {
                    apps::set_volume(&s.app.audio_id(), v, Some(false));
                    self.volume.set((t, now_ms(), Some((v, false))));
                }
            }
            (Act::ToggleMute, Some(t)) => {
                if let Some(id) = self.hub.get(t).map(|s| s.app.audio_id()) {
                    if let Some((level, muted)) = apps::volume(&id) {
                        apps::set_volume(&id, level, Some(!muted));
                        self.volume.set((t, now_ms(), Some((level, !muted))));
                    }
                    self.changed();
                }
            }
            (Act::Pin(k), _) => {
                self.hub.pin(k);
                self.changed();
                self.popup.flash(Flash::Switched);
                self.popup.restart_hide(self.settings.popup_seconds);
            }
            (Act::OpenApp, Some(t)) => self.open_app(t),
            (Act::CopySong, Some(t)) => {
                if let Some((title, artist)) = self.hub.get(t).map(|s| (s.title.clone(), s.artist.clone())).filter(|x| !x.0.trim().is_empty()) {
                    let ok = self.copy_song(&title, &artist);
                    self.notice(if ok { "Copied song name" } else { "Couldn't copy (clipboard busy)" });
                }
            }
            (Act::TogglePreferred, Some(t)) => {
                let Some((id, name)) = self.hub.get(t).map(|s| (s.app.aumid.clone(), s.app.name.clone())) else { return };
                let was = self.settings.prefers(&id);
                self.settings.preferred_app = (!was).then_some(id);
                self.save_settings();
                self.notice(&if was { "No preferred app".to_string() } else { format!("{name} is now preferred") });
            }
            (Act::Snooze, _) => {
                self.snooze_until = now_ms() + 60 * 60 * 1000;
                self.popup.hide();
            }
            (Act::Settings, _) => {
                self.popup.hide();
                self.show_settings();
            }
            _ => {}
        }
    }

    // ---------- key gestures ----------

    fn context_target(&self) -> Option<logic::Target> {
        let s = self.hub.get(self.hub.target(&self.settings)?)?;
        Some(logic::Target { key: s.key, can_seek: s.can_seek, position: s.position(), duration: s.timeline.duration })
    }

    fn key(&mut self, key: Key, down: bool) {
        let cx = logic::Context { settings: &self.settings, target: self.context_target(), apps: self.hub.controllable(&self.settings).count(), now_ms: now_ms() };
        let dos = if down { self.gestures.down(key, &cx) } else { self.gestures.up(key, &cx) };
        self.perform(dos);
    }

    fn perform(&mut self, dos: Vec<Do>) {
        for d in dos {
            match d {
                Do::Timer(ms) => self.timer(TIMER_HOLD, ms as u32),
                Do::StopTimer => self.stop_timer(TIMER_HOLD),
                Do::Command(key, t) => {
                    let ours = self.hub.get(t).map(|s| s.app.aumid.clone());
                    let better = self.hub.windows_choice().map(|a| a.to_lowercase()) != ours.map(|a| a.to_lowercase());
                    self.count(|s| s.key(better, Local::now()));
                    self.command(key, t);
                    self.flash(match key { Key::PlayPause => Flash::PlayPause, Key::Next => Flash::Next, Key::Previous => Flash::Previous, Key::Stop => Flash::Stop }, false);
                }
                Do::NothingToControl => self.flash(Flash::None, false),
                Do::OpenInstead(t) => {
                    // Undo the first tap's play/pause and bring the app forward instead.
                    self.command(Key::PlayPause, t);
                    self.open_app(t);
                }
                Do::SwitchApp => {
                    self.hub.cycle(&self.settings);
                    self.changed();
                    self.flash(Flash::Switched, false);
                    self.count(|s| {
                        s.app_switches += 1;
                        s.key(false, Local::now());
                    });
                }
                Do::ShowScrub { position, from, .. } => {
                    if self.popup_allowed() {
                        self.show_popup();
                        self.popup.show_scrub(position, from);
                        self.popup.restart_hide(self.settings.popup_seconds);
                    }
                }
                Do::Seek(t, p) => self.hub.seek(t, p),
                Do::ScrubDone => {
                    self.popup.end_scrub();
                    self.changed();
                    self.count(|s| {
                        s.scrubs += 1;
                        s.key(false, Local::now());
                    });
                }
            }
        }
    }

    // ---------- settings window ----------

    fn model(&mut self) -> ui::Model {
        let target = self.hub.target(&self.settings).and_then(|k| self.hub.get(k));
        let n = self.hub.sessions.len();
        let (status_art, status_title, status_sub) = match target {
            None if n == 0 => (None, "Listening for media keys".to_string(), "No app is reporting media right now".to_string()),
            None => (None, "All media apps are turned off below".to_string(), "Keys go to Windows as usual".to_string()),
            Some(t) => (
                t.art.clone().or_else(|| t.app.icon.clone()),
                if t.title.trim().is_empty() { t.app.name.clone() } else { t.title.clone() },
                format!("Keys control {} · {}{}", t.app.name, if t.playing { "Playing" } else { "Paused" }, if n > 1 { format!(" · {n} apps with media") } else { String::new() }),
            ),
        };
        // running apps first, then by name
        let mut known: Vec<(String, String)> = self.settings.known_apps.clone().into_iter().collect();
        let live = |id: &str| self.hub.sessions.iter().find(|s| config::same_app(&s.app.aumid, id));
        known.sort_by_key(|(id, name)| (live(id).is_none(), name.to_lowercase()));
        let app_rows = known
            .iter()
            .map(|(id, name)| {
                let session = live(id);
                let mut sub = match session {
                    None => "Not running".to_string(),
                    Some(s) if s.playing => "Playing now".to_string(),
                    Some(_) => "Open, paused".to_string(),
                };
                if self.settings.prefers(id) {
                    sub = format!("Preferred · {sub}");
                }
                if let Some(s) = session.filter(|s| !s.title.trim().is_empty()) {
                    sub = format!("{sub} · {}", s.title);
                }
                ui::AppRow { id: id.clone(), name: name.clone(), icon: apps::info(id).icon.clone(), sub, preferred: self.settings.prefers(id), enabled: !self.settings.ignores(id) }
            })
            .collect::<Vec<_>>();
        self.app_rows = app_rows.iter().map(|a| a.id.clone()).collect();
        let now = Local::now();
        let history = self
            .history
            .iter()
            .take(20)
            .map(|h| ui::TrackRow {
                icon: apps::info(&h.aumid).icon.clone(),
                title: h.title.clone(),
                sub: format!("{} · {}", if h.artist.trim().is_empty() { &h.app } else { &h.artist }, ago(h.at, now)),
            })
            .collect();
        let st = &self.stats;
        let update_status = updater::STATUS.lock().unwrap().clone();
        ui::Model {
            settings: self.settings.clone(),
            starts_with_windows: system::autostart_value().is_some(),
            welcome: !self.settings.welcome_shown,
            status_art,
            status_title,
            status_sub,
            apps: app_rows,
            history,
            stats: vec![
                (thousands(st.keys_today), "key presses today"),
                (thousands(st.keys_handled), "key presses in total"),
                (thousands(st.better_than_windows), "went to a different app than Windows would have"),
                (thousands(st.scrubs), "scrubs"),
                (thousands(st.app_switches), "app switches"),
                (thousands(st.hook_recoveries), "times the key hook was reconnected"),
            ],
            stats_since: format!("Counting since {}. {} key presses were handed to players that don't report to Windows.", st.since.format("%-d %b %Y"), thousands(st.passed_to_other_players)),
            update_status: if !update_status.is_empty() {
                update_status
            } else if updater::can_self_update() {
                "Checks GitHub every few hours and installs quietly.".into()
            } else {
                "This copy is a development build and doesn't update itself.".into()
            },
            logo: Some(self.logo.clone()),
        }
    }

    fn show_settings(&mut self) {
        match &self.window {
            Some(w) => w.activate(),
            None => {
                let model = self.model();
                self.window = Some(ui::Window::open(model));
            }
        }
    }

    fn refresh_window(&mut self) {
        if self.window.is_some() {
            let model = self.model();
            if let Some(w) = self.window.as_mut() {
                w.update(model);
            }
        }
    }

    fn close_settings(&mut self) {
        if let Some(w) = self.window.take() {
            w.close();
        }
        if !self.settings.welcome_shown {
            self.settings.welcome_shown = true;
            config::save("settings.json", &self.settings);
        }
    }

    fn settings_act(&mut self, act: Action) {
        match act {
            Action::Set(which, on) => {
                let s = &mut self.settings;
                match which {
                    Toggle::ShowPopup => s.show_popup = on,
                    Toggle::Fullscreen => s.hide_in_fullscreen = on,
                    Toggle::Scrub => s.hold_to_scrub = on,
                    Toggle::HoldSwitch => s.hold_to_switch_app = on,
                    Toggle::DoubleTap => s.double_tap_to_open = on,
                    Toggle::AlwaysCapture => s.always_capture = on,
                    Toggle::PassThrough => s.pass_through_unknown_players = on,
                    Toggle::AutoUpdate => s.auto_update = on,
                    Toggle::Startup => system::set_autostart(on),
                }
                self.save_settings();
            }
            Action::Place(p) => {
                self.settings.placement = p;
                self.save_settings();
                self.popup.hide_now();
                self.flash(Flash::None, true);
            }
            Action::FollowMouse(v) => {
                self.settings.follow_mouse = v;
                self.save_settings();
            }
            Action::Duration(v) => {
                if self.settings.popup_seconds != v {
                    self.settings.popup_seconds = v;
                    self.save_settings();
                }
            }
            Action::Skip(n) => {
                self.settings.skip_seconds = n;
                self.save_settings();
            }
            Action::Prefer(i) => {
                if let Some(id) = self.app_rows.get(i).cloned() {
                    self.settings.preferred_app = (!self.settings.prefers(&id)).then_some(id);
                    self.save_settings();
                }
            }
            Action::AppEnabled(i, on) => {
                if let Some(id) = self.app_rows.get(i).cloned() {
                    self.settings.set_ignored(&id, !on);
                    self.save_settings();
                }
            }
            Action::ClearHistory => {
                self.history.clear();
                config::save("history.json", &self.history);
                self.refresh_window();
            }
            Action::YouTube(i) | Action::Spotify(i) | Action::Copy(i) => {
                let Some((title, artist)) = self.history.get(i).map(|h| (h.title.clone(), h.artist.clone())) else { return };
                let query = system::percent_encode(&if artist.trim().is_empty() { title.clone() } else { format!("{artist} {title}") });
                match act {
                    Action::YouTube(_) => system::open_url(&format!("https://www.youtube.com/results?search_query={query}")),
                    Action::Spotify(_) => system::open_url(&format!("https://open.spotify.com/search/{query}")),
                    _ => {
                        self.copy_song(&title, &artist);
                    }
                }
            }
            Action::CheckUpdates => updater::check(true),
            Action::Preview => self.flash(Flash::None, true),
            Action::Quit => {
                config::log("Quit from settings.");
                self.close_settings();
                self.quit = true;
            }
            Action::Close => self.close_settings(),
        }
    }

    // ---------- events ----------

    fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key, down) => self.key(key, down),
            Event::Manager(m) => {
                self.hub.connect(m);
                self.changed();
            }
            Event::Sessions => {
                self.hub.sync();
                self.changed();
            }
            Event::CurrentSession => self.changed(),
            Event::Props(k) => self.hub.request_props(k),
            Event::PropsRead(p) => {
                self.hub.apply_props(p);
                self.changed();
            }
            Event::Playback(k) => {
                self.hub.refresh_playback(k);
                self.changed();
            }
            Event::Timeline(k) => {
                self.hub.refresh_timeline(k);
                // Positions only matter on screen.
                if self.popup.visible() {
                    let view = self.view();
                    self.popup.set_view(view);
                }
            }
            Event::Timer(id) => self.on_timer(id),
            Event::Popup(msg, wp, lp) => {
                for act in self.popup.message(msg, wp, lp) {
                    self.popup_act(act);
                }
            }
            Event::Settings(msg, wp, lp) => {
                let acts = self.window.as_mut().map_or_else(Vec::new, |w| w.message(msg, wp, lp));
                for act in acts {
                    self.settings_act(act);
                }
            }
            Event::RawMediaKey => {
                self.raw_seen = now_ms();
                self.timer(TIMER_RAW_CHECK, 250); // raw input and the hook can arrive in either order
            }
            Event::Resumed => self.rehook(),
            Event::Signal(Signal::ShowSettings) => self.show_settings(),
            Event::Signal(Signal::Preview) => self.flash(Flash::None, true),
            Event::Signal(Signal::DropHook) => {
                if self.test {
                    hook::BLIND.store(true, Relaxed);
                    config::log("Test: hook dropped.");
                }
            }
            Event::UpdateStatus => self.refresh_window(),
            Event::UpdateReady => self.timer(TIMER_UPDATE_APPLY, 2000),
        }
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_HOLD => {
                let dos = self.gestures.tick(now_ms());
                self.perform(dos);
            }
            TIMER_REHOOK => self.rehook(),
            TIMER_FLUSH => self.flush_stats(),
            TIMER_UPDATE_CHECK => {
                self.timer(TIMER_UPDATE_CHECK, 6 * 3600 * 1000);
                if self.settings.auto_update {
                    updater::check(false);
                }
            }
            // Install when nothing is on screen and no key is held.
            TIMER_UPDATE_APPLY => {
                if !self.popup.visible() && !self.gestures.holding() && self.window.is_none() {
                    self.stop_timer(TIMER_UPDATE_APPLY);
                    self.flush_stats();
                    hook::stop();
                    // The new copy waits for this process to end.
                    if updater::apply_and_restart() {
                        self.quit = true;
                    } else {
                        self.start_hook();
                    }
                }
            }
            TIMER_RAW_CHECK => {
                self.stop_timer(TIMER_RAW_CHECK);
                let missed = self.raw_seen.saturating_sub(hook::LAST_SEEN.load(Relaxed)) > 600;
                if missed && self.raw_seen.saturating_sub(self.last_recovery) > 2000 {
                    self.last_recovery = self.raw_seen;
                    config::log("Keyboard hook stopped receiving keys; reconnected it.");
                    hook::BLIND.store(false, Relaxed);
                    self.count(|s| s.hook_recoveries += 1);
                    self.rehook();
                }
            }
            TIMER_RESYNC_SOON | TIMER_RESYNC_LATER => {
                self.stop_timer(id);
                if let Some(k) = self.resync {
                    self.hub.refresh(k);
                    self.changed();
                }
            }
            _ => {}
        }
    }
}

pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn ago(then: DateTime<Local>, now: DateTime<Local>) -> String {
    let d = now - then;
    if d.num_minutes() < 1 {
        "just now".into()
    } else if d.num_minutes() < 60 {
        format!("{} min ago", d.num_minutes())
    } else if d.num_hours() < 24 {
        format!("{} h ago", d.num_hours())
    } else if d.num_days() < 7 {
        then.format("%A %H:%M").to_string()
    } else {
        then.format("%-d %b").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wording() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234567), "1,234,567");
        let now = Local::now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now - chrono::Duration::minutes(5), now), "5 min ago");
        assert_eq!(ago(now - chrono::Duration::hours(3), now), "3 h ago");
    }
}
