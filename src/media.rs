//! Every app that reports media to Windows (Spotify, a browser, a player…), kept up to date on the
//! UI thread. Windows calls back on its own threads; those callbacks only post an event here.

use crate::apps::{self, AppInfo};
use crate::art::{self, Image};
use crate::config::Settings;
use crate::logic::{self, Candidate, Timeline};
use crate::system::now_ms;
use crate::{Event, post};
use std::rc::Rc;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Imaging::*;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as GSession, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{IRandomAccessStream, IRandomAccessStreamReference};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows_core::Interface;
use windows_future::IAsyncOperation;

const TICKS: f64 = 10_000_000.0;

pub struct Session {
    pub key: u64,
    remote: GSession,
    tokens: [i64; 3],
    pub app: Rc<AppInfo>,
    pub title: String,
    pub artist: String,
    pub art: Option<Rc<Image>>,
    pub accent: u32,
    pub playing: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_seek: bool,
    can_stop: bool,
    pub timeline: Timeline,
    /// Where the app's own timeline starts, in seconds.
    start: f64,
    pub started_at: u64,
    pub last_played_at: u64,
    props_version: u32,
}

impl Session {
    pub fn position(&self) -> f64 {
        self.timeline.now(self.playing, now_ms())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let [a, b, c] = self.tokens;
        let _ = self.remote.RemoveMediaPropertiesChanged(a);
        let _ = self.remote.RemovePlaybackInfoChanged(b);
        let _ = self.remote.RemoveTimelinePropertiesChanged(c);
    }
}

fn hook(s: &GSession, key: u64) -> [i64; 3] {
    [
        s.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
            post(Event::Props(key));
            Ok(())
        }))
        .unwrap_or(0),
        s.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
            post(Event::Playback(key));
            Ok(())
        }))
        .unwrap_or(0),
        s.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
            post(Event::Timeline(key));
            Ok(())
        }))
        .unwrap_or(0),
    ]
}

/// A track's details, read on a worker thread (the calls wait on the other app).
pub struct Props {
    pub key: u64,
    version: u32,
    title: String,
    artist: String,
    /// `None`: keep what we have. `Some(None)`: the track has no art.
    art: Option<Option<(Image, u32)>>,
}

fn read_art(thumb: &IRandomAccessStreamReference) -> windows_core::Result<Image> {
    let stream: IRandomAccessStream = thumb.OpenReadAsync()?.join()?.cast()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (w, h) = (decoder.PixelWidth()?.max(1), decoder.PixelHeight()?.max(1));
    let (nw, nh) = (320, (h * 320 / w).max(1));
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(nw)?;
    transform.SetScaledHeight(nh)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    let data = decoder
        .GetPixelDataTransformedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied, &transform, ExifOrientationMode::RespectExifOrientation, ColorManagementMode::DoNotColorManage)?
        .join()?
        .DetachPixelData()?;
    Ok(Image { w: nw, h: nh, px: data.to_vec() }.trim_letterbox())
}

fn read_props(remote: &GSession, key: u64, version: u32, old: (&str, &str), has_art: bool) -> windows_core::Result<Props> {
    let p = remote.TryGetMediaPropertiesAsync()?.join()?;
    let title = p.Title()?.to_string();
    let mut artist = p.Artist()?.to_string();
    if artist.is_empty() {
        artist = p.AlbumArtist()?.to_string();
    }
    let art = (title != old.0 || artist != old.1 || !has_art).then(|| {
        p.Thumbnail().ok().and_then(|t| read_art(&t).ok()).map(|i| {
            let accent = art::accent(&i);
            (i, accent)
        })
    });
    Ok(Props { key, version, title, artist, art })
}

/// Asks Windows for the session manager without blocking the UI thread.
pub fn request_manager() {
    std::thread::spawn(|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        match Manager::RequestAsync().and_then(|op| op.join()) {
            Ok(m) => post(Event::Manager(m)),
            Err(e) => crate::config::log(&format!("Media sessions unavailable: {e}")),
        }
    });
}

#[derive(Default)]
pub struct Hub {
    manager: Option<Manager>,
    /// In the order the pop-up shows them.
    pub sessions: Vec<Session>,
    pinned: Option<(u64, u64)>,
    next_key: u64,
    /// Tracks that started since the last `take_started` (title, artist, app).
    started: Vec<(String, String, Rc<AppInfo>)>,
}

impl Hub {
    pub fn connect(&mut self, manager: Manager) {
        let _ = manager.SessionsChanged(&TypedEventHandler::new(|_, _| {
            post(Event::Sessions);
            Ok(())
        }));
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(|_, _| {
            post(Event::CurrentSession);
            Ok(())
        }));
        self.manager = Some(manager);
        self.sync();
        // Sessions already playing at start-up: prefer the one Windows considers current.
        let current = self.windows_choice();
        let now = now_ms();
        for s in self.sessions.iter_mut().filter(|s| s.playing) {
            s.started_at = if current.as_deref() == Some(s.app.aumid.as_str()) { now } else { now.saturating_sub(1) };
            s.last_played_at = s.started_at;
        }
    }

    /// Matches our list to Windows' (by app ID, in order, when an app has several sessions).
    pub fn sync(&mut self) {
        let Some(manager) = &self.manager else { return };
        let Ok(live) = manager.GetSessions() else { return };
        let mut seen = Vec::new();
        let mut fresh = Vec::new();
        for remote in live {
            let aumid = remote.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default();
            if let Some(s) = self.sessions.iter_mut().find(|s| s.app.aumid == aumid && !seen.contains(&s.key)) {
                // Windows sometimes hands out a new session object for the same app.
                if s.remote.as_raw() != remote.as_raw() {
                    let [a, b, c] = s.tokens;
                    let _ = s.remote.RemoveMediaPropertiesChanged(a);
                    let _ = s.remote.RemovePlaybackInfoChanged(b);
                    let _ = s.remote.RemoveTimelinePropertiesChanged(c);
                    s.tokens = hook(&remote, s.key);
                    s.remote = remote;
                }
                seen.push(s.key);
                continue;
            }
            self.next_key += 1;
            let key = self.next_key;
            self.sessions.push(Session {
                key,
                tokens: hook(&remote, key),
                remote,
                app: apps::info(&aumid),
                title: String::new(),
                artist: String::new(),
                art: None,
                accent: art::DEFAULT_ACCENT,
                playing: false,
                can_previous: false,
                can_next: false,
                can_seek: false,
                can_stop: false,
                timeline: Timeline::default(),
                start: 0.0,
                started_at: 0,
                last_played_at: 0,
                props_version: 0,
            });
            seen.push(key);
            fresh.push(key);
        }
        self.sessions.retain(|s| seen.contains(&s.key));
        if self.pinned.is_some_and(|(k, _)| !seen.contains(&k)) {
            self.pinned = None;
        }
        for key in fresh {
            self.refresh(key);
        }
    }

    pub fn get(&self, key: u64) -> Option<&Session> {
        self.sessions.iter().find(|s| s.key == key)
    }

    fn get_mut(&mut self, key: u64) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.key == key)
    }

    pub fn refresh(&mut self, key: u64) {
        self.refresh_playback(key);
        self.refresh_timeline(key);
        self.request_props(key);
    }

    pub fn request_props(&mut self, key: u64) {
        let Some(s) = self.get_mut(key) else { return };
        s.props_version += 1;
        let (remote, version, title, artist, has_art) = (s.remote.clone(), s.props_version, s.title.clone(), s.artist.clone(), s.art.is_some());
        std::thread::spawn(move || {
            unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            if let Ok(p) = read_props(&remote, key, version, (&title, &artist), has_art) {
                post(Event::PropsRead(p));
            }
        });
    }

    pub fn apply_props(&mut self, p: Props) {
        let Some(s) = self.get_mut(p.key) else { return };
        if s.props_version != p.version {
            return; // a newer read is on its way
        }
        let changed = p.title != s.title || p.artist != s.artist;
        s.title = p.title;
        s.artist = p.artist;
        match p.art {
            Some(Some((image, accent))) => (s.art, s.accent) = (Some(Rc::new(image)), accent),
            Some(None) => (s.art, s.accent) = (None, art::DEFAULT_ACCENT),
            None => {}
        }
        if changed && s.playing {
            let started = (s.title.clone(), s.artist.clone(), s.app.clone());
            self.started.push(started);
        }
    }

    pub fn refresh_playback(&mut self, key: u64) {
        let Some(s) = self.get_mut(key) else { return };
        let Ok(info) = s.remote.GetPlaybackInfo() else { return };
        let playing = info.PlaybackStatus().is_ok_and(|x| x == Status::Playing);
        if let Ok(c) = info.Controls() {
            s.can_previous = c.IsPreviousEnabled().unwrap_or(false);
            s.can_next = c.IsNextEnabled().unwrap_or(false);
            s.can_seek = c.IsPlaybackPositionEnabled().unwrap_or(false);
            s.can_stop = c.IsStopEnabled().unwrap_or(false);
        }
        if let Some(rate) = info.PlaybackRate().ok().and_then(|r| r.Value().ok()).filter(|r| *r > 0.0) {
            s.timeline.rate = rate;
        }
        let was = s.playing;
        let now = now_ms();
        if playing || was {
            s.last_played_at = now;
        }
        if playing != was {
            // Freeze (or restart) the extrapolated position at the moment the state changed.
            s.timeline.position = s.timeline.now(was, now);
            s.timeline.at_ms = now;
            s.playing = playing;
            if playing {
                s.started_at = now;
                let started = (s.title.clone(), s.artist.clone(), s.app.clone());
                self.started.push(started);
            }
        }
    }

    pub fn refresh_timeline(&mut self, key: u64) {
        let Some(s) = self.get_mut(key) else { return };
        let Ok(t) = s.remote.GetTimelineProperties() else { return };
        let secs = |v: windows_core::Result<windows::Foundation::TimeSpan>| v.map_or(0.0, |v| v.Duration as f64 / TICKS);
        s.start = secs(t.StartTime());
        let mut end = secs(t.EndTime());
        if end <= 0.0 {
            end = secs(t.MaxSeekTime());
        }
        s.timeline.duration = (end - s.start).max(0.0);
        s.timeline.position = (secs(t.Position()) - s.start).max(0.0);
        // Some apps report a stale or zero timestamp; then trust our own clock.
        let now_file = unsafe {
            let f = GetSystemTimeAsFileTime();
            ((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64
        };
        let age = t.LastUpdatedTime().map_or(0.0, |at| (now_file - at.UniversalTime) as f64 / TICKS);
        let age = if (-5.0..=12.0 * 3600.0).contains(&age) { age.max(0.0) } else { 0.0 };
        s.timeline.at_ms = now_ms().saturating_sub((age * 1000.0) as u64).max(1);
    }

    pub fn take_started(&mut self) -> Vec<(String, String, Rc<AppInfo>)> {
        std::mem::take(&mut self.started)
    }

    // ---------- which app ----------

    pub fn controllable<'a>(&'a self, settings: &'a Settings) -> impl Iterator<Item = &'a Session> {
        self.sessions.iter().filter(|s| !settings.ignores(&s.app.aumid))
    }

    fn candidates(&self, settings: &Settings) -> Vec<Candidate> {
        self.controllable(settings)
            .map(|s| Candidate { key: s.key, aumid: s.app.aumid.clone(), playing: s.playing, started_at: s.started_at, last_played_at: s.last_played_at })
            .collect()
    }

    pub fn target(&self, settings: &Settings) -> Option<u64> {
        logic::target(&self.candidates(settings), self.pinned, settings, || self.windows_choice())
    }

    /// The app Windows itself would have sent the key to.
    pub fn windows_choice(&self) -> Option<String> {
        let s = self.manager.as_ref()?.GetCurrentSession().ok()?;
        s.SourceAppUserModelId().ok().map(|s| s.to_string())
    }

    /// Control this app until another one starts playing.
    pub fn pin(&mut self, key: u64) {
        self.pinned = Some((key, now_ms()));
    }

    /// Switches to the next app, in the order they appear on the pop-up.
    pub fn cycle(&mut self, settings: &Settings) {
        let list = self.candidates(settings);
        if let Some(next) = logic::next_target(&list, self.target(settings)).filter(|_| list.len() > 1) {
            self.pin(next);
        }
    }

    // ---------- commands ----------

    /// Shows the new state right away, before the app confirms it.
    fn assume(&mut self, key: u64, playing: bool) {
        let Some(s) = self.get_mut(key) else { return };
        if s.playing == playing {
            return;
        }
        let now = now_ms();
        s.timeline.position = s.timeline.now(s.playing, now);
        s.timeline.at_ms = now;
        s.playing = playing;
        if playing {
            s.started_at = now;
        }
        s.last_played_at = now;
    }

    pub fn play_pause(&mut self, key: u64) {
        let Some(s) = self.get(key) else { return };
        let (remote, playing) = (s.remote.clone(), s.playing);
        self.assume(key, !playing);
        let first = if playing { remote.TryPauseAsync() } else { remote.TryPlayAsync() };
        or_else(first, move || remote.TryTogglePlayPauseAsync());
    }

    pub fn next(&self, key: u64) {
        if let Some(s) = self.get(key) {
            let _ = s.remote.TrySkipNextAsync();
        }
    }

    pub fn previous(&self, key: u64) {
        if let Some(s) = self.get(key) {
            let _ = s.remote.TrySkipPreviousAsync();
        }
    }

    pub fn stop(&mut self, key: u64) {
        let Some(s) = self.get(key) else { return };
        let (remote, can_stop) = (s.remote.clone(), s.can_stop);
        self.assume(key, false);
        if can_stop {
            or_else(remote.TryStopAsync(), move || remote.TryPauseAsync());
        } else {
            let _ = remote.TryPauseAsync();
        }
    }

    /// Seeks to `position` seconds from the start of the track.
    pub fn seek(&mut self, key: u64, position: f64) {
        let Some(s) = self.get_mut(key).filter(|s| s.can_seek) else { return };
        let mut p = position.max(0.0);
        if s.timeline.duration > 0.0 && p > s.timeline.duration {
            p = (s.timeline.duration - 0.5).max(0.0);
        }
        s.timeline.position = p;
        s.timeline.at_ms = now_ms();
        let _ = s.remote.TryChangePlaybackPositionAsync(((s.start + p) * TICKS) as i64);
    }
}

/// Runs `fallback` if the app refuses (or fails) the first request.
fn or_else(first: windows_core::Result<IAsyncOperation<bool>>, fallback: impl FnOnce() -> windows_core::Result<IAsyncOperation<bool>> + Send + 'static) {
    if let Ok(op) = first {
        let _ = op.when(move |ok| {
            if !ok.unwrap_or(false) {
                let _ = fallback();
            }
        });
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};
    use windows::Foundation::TimeSpan;
    use windows::Media::*;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows_core::{HSTRING, w};

    const TITLE: &str = "Tempo test tone";

    /// A media session of our own, as a player would publish it; reports the commands it receives.
    fn fake_player() -> (SystemMediaTransportControls, Receiver<String>) {
        let (tx, rx) = channel::<String>();
        unsafe {
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("Tempo test player"), WS_OVERLAPPEDWINDOW, 0, 0, 10, 10, None, None, None, None).unwrap();
            let interop = windows_core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>().unwrap();
            let smtc: SystemMediaTransportControls = interop.GetForWindow(hwnd).unwrap();
            smtc.SetIsEnabled(true).unwrap();
            smtc.SetIsPlayEnabled(true).unwrap();
            smtc.SetIsPauseEnabled(true).unwrap();
            smtc.SetIsStopEnabled(true).unwrap();
            smtc.SetIsNextEnabled(true).unwrap();
            smtc.SetIsPreviousEnabled(true).unwrap();
            let display = smtc.DisplayUpdater().unwrap();
            display.SetType(MediaPlaybackType::Music).unwrap();
            display.MusicProperties().unwrap().SetTitle(&HSTRING::from(TITLE)).unwrap();
            display.MusicProperties().unwrap().SetArtist(&HSTRING::from("Tempo")).unwrap();
            display.Update().unwrap();
            smtc.SetPlaybackStatus(MediaPlaybackStatus::Playing).unwrap();
            timeline(&smtc, 0);
            let (pressed, me) = (tx.clone(), smtc.clone());
            smtc.ButtonPressed(&TypedEventHandler::new(move |_, args: windows_core::Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                let (name, status) = match args.ok()?.Button()? {
                    SystemMediaTransportControlsButton::Play => ("play", Some(MediaPlaybackStatus::Playing)),
                    SystemMediaTransportControlsButton::Pause => ("pause", Some(MediaPlaybackStatus::Paused)),
                    SystemMediaTransportControlsButton::Stop => ("stop", Some(MediaPlaybackStatus::Stopped)),
                    SystemMediaTransportControlsButton::Next => ("next", None),
                    SystemMediaTransportControlsButton::Previous => ("previous", None),
                    _ => ("other", None),
                };
                if let Some(s) = status {
                    me.SetPlaybackStatus(s)?;
                }
                let _ = pressed.send(name.to_string());
                Ok(())
            }))
            .unwrap();
            let me = smtc.clone();
            smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(move |_, args: windows_core::Ref<PlaybackPositionChangeRequestedEventArgs>| {
                let to = args.ok()?.RequestedPlaybackPosition()?.Duration / TICKS as i64;
                timeline(&me, to);
                let _ = tx.send(format!("seek {to}"));
                Ok(())
            }))
            .unwrap();
            (smtc, rx)
        }
    }

    fn timeline(smtc: &SystemMediaTransportControls, position: i64) {
        let at = |s: i64| TimeSpan { Duration: s * TICKS as i64 };
        let t = SystemMediaTransportControlsTimelineProperties::new().unwrap();
        t.SetStartTime(at(0)).unwrap();
        t.SetMinSeekTime(at(0)).unwrap();
        t.SetPosition(at(position)).unwrap();
        t.SetMaxSeekTime(at(60)).unwrap();
        t.SetEndTime(at(60)).unwrap();
        smtc.UpdateTimelineProperties(&t).unwrap();
    }

    /// Handles window messages and the events Windows sends until `done`, or fails after 8 s.
    fn until(hub: &mut Hub, what: &str, mut done: impl FnMut(&mut Hub) -> bool) {
        let start = Instant::now();
        while !done(hub) {
            assert!(start.elapsed() < Duration::from_secs(8), "timed out waiting for: {what}");
            unsafe {
                let mut m = MSG::default();
                while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&m);
                }
            }
            while let Some(e) = crate::next_event() {
                match e {
                    Event::Sessions => hub.sync(),
                    Event::Props(k) => hub.request_props(k),
                    Event::PropsRead(p) => hub.apply_props(p),
                    Event::Playback(k) => hub.refresh_playback(k),
                    Event::Timeline(k) => hub.refresh_timeline(k),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        println!("ok: {what}");
    }

    /// `cargo test controls_a_real_session -- --ignored` in a signed-in desktop session. It
    /// publishes its own media session and only ever sends commands to that one. (A session made
    /// on any other desktop than the normal one is invisible to Windows.)
    #[test]
    #[ignore]
    fn controls_a_real_session() {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let (_player, commands) = fake_player();
        let mut hub = Hub::default();
        hub.connect(Manager::RequestAsync().unwrap().join().unwrap());
        let find = |hub: &Hub| hub.sessions.iter().find(|s| s.title == TITLE).map(|s| s.key);
        until(&mut hub, "our session appears with its title", |h| find(h).is_some());
        let key = find(&hub).unwrap();
        until(&mut hub, "it is playing and seekable", |h| h.get(key).is_some_and(|s| s.playing && s.can_next && s.timeline.duration > 50.0));
        assert_eq!(hub.get(key).unwrap().artist, "Tempo");
        let expect = |hub: &mut Hub, command: &str| {
            let mut seen = Vec::new();
            until(hub, &format!("the player receives '{command}'"), |_| {
                seen.extend(commands.try_iter());
                seen.iter().any(|c| c == command)
            });
        };
        hub.play_pause(key);
        assert!(!hub.get(key).unwrap().playing, "shown as paused at once");
        expect(&mut hub, "pause");
        until(&mut hub, "Windows reports it paused", |h| {
            h.refresh_playback(key);
            !h.get(key).unwrap().playing
        });
        hub.play_pause(key);
        expect(&mut hub, "play");
        until(&mut hub, "Windows reports it playing", |h| {
            h.refresh_playback(key);
            h.get(key).unwrap().playing
        });
        hub.next(key);
        expect(&mut hub, "next");
        hub.previous(key);
        expect(&mut hub, "previous");
        if hub.get(key).unwrap().can_seek {
            hub.seek(key, 30.0);
            expect(&mut hub, "seek 30");
            until(&mut hub, "the new position is read back", |h| h.get(key).is_some_and(|s| (29.0..36.0).contains(&s.position())));
        } else {
            println!("not checked: seeking (Windows says this session can't seek)");
        }
        hub.stop(key);
        expect(&mut hub, "stop");
    }
}
