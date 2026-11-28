//! The card that slides in on a media key press and fades away on its own: a per-pixel transparent
//! window that never takes focus. It only draws and reports what was clicked; `app` does the rest.

use crate::art::{self, Image};
use crate::config::Placement;
use crate::gfx::*;
use crate::logic::{Timeline, format_time};
use crate::system::{self, now_ms, wide};
use std::rc::Rc;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::PCWSTR;

pub const CARD_W: f32 = 404.0;
pub const CARD_H: f32 = 202.0;
/// Transparent room around the card for its shadow.
pub const MARGIN: f32 = 28.0;

const FG: u32 = 0xFFF2F3F7;
const MUTED: u32 = 0xFFA3A6B4;
const DARK: u32 = 0xFF111216;

const PLAY: &str = "M8 5.14v13.72c0 .79.87 1.27 1.54.84l10.4-6.86a1 1 0 0 0 0-1.68L9.54 4.3A1 1 0 0 0 8 5.14z";
const PAUSE: &str = "M7.5 5h2a1.5 1.5 0 0 1 1.5 1.5v11a1.5 1.5 0 0 1-1.5 1.5h-2A1.5 1.5 0 0 1 6 17.5v-11A1.5 1.5 0 0 1 7.5 5zM14.5 5h2a1.5 1.5 0 0 1 1.5 1.5v11a1.5 1.5 0 0 1-1.5 1.5h-2a1.5 1.5 0 0 1-1.5-1.5v-11A1.5 1.5 0 0 1 14.5 5z";
const NEXT: &str = "M4.5 6.2v11.6c0 .8.9 1.3 1.6.8l8.2-5.8a1 1 0 0 0 0-1.6L6.1 5.4c-.7-.5-1.6 0-1.6.8zM17 5.5h1.5a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H17a1 1 0 0 1-1-1v-11a1 1 0 0 1 1-1z";
const BACK: &str = "M12 4.5V2.2c0-.4-.5-.7-.8-.4L7.6 5.1c-.3.2-.3.6 0 .8l3.6 3.3c.3.3.8 0 .8-.4V6.5a6 6 0 1 1-6 6.2 1 1 0 0 0-2 0A8 8 0 1 0 12 4.5z";
const SPEAKER: &str = "M3.5 9.2h3.3l4.3-3.6c.6-.5 1.4-.1 1.4.7v11.4c0 .8-.8 1.2-1.4.7l-4.3-3.6H3.5a1 1 0 0 1-1-1V10.2a1 1 0 0 1 1-1z";
const WAVES: &str = "M15.6 9.1a4.2 4.2 0 0 1 0 5.8M18.4 6.4a8 8 0 0 1 0 11.2";
const MUTE_X: &str = "M15.5 9.5l5 5M20.5 9.5l-5 5";

#[derive(Clone, Default)]
pub struct Track {
    pub key: u64,
    pub app: String,
    pub icon: Option<Rc<Image>>,
    pub title: String,
    pub artist: String,
    pub art: Option<Rc<Image>>,
    pub accent: u32,
    pub playing: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_seek: bool,
    pub timeline: Timeline,
}

#[derive(Clone)]
pub struct Source {
    pub key: u64,
    pub name: String,
    pub title: String,
    pub icon: Option<Rc<Image>>,
    pub playing: bool,
}

/// Everything the card shows.
#[derive(Clone, Default)]
pub struct View {
    pub target: Option<Track>,
    pub sources: Vec<Source>,
    pub volume: Option<(f32, bool)>,
    pub skip: u32,
    /// The target is the preferred app.
    pub preferred: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Flash {
    None,
    PlayPause,
    Next,
    Previous,
    Stop,
    Switched,
}

/// What the user did on the card.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    PlayPause,
    Next,
    Previous,
    Skip(f64),
    Seek(f64),
    Volume(f32),
    ToggleMute,
    Pin(u64),
    OpenApp,
    CopySong,
    TogglePreferred,
    Snooze,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Card,
    Art,
    Settings,
    Copy,
    Source(u64),
    Progress,
    Back,
    Previous,
    Play,
    Next,
    Forward,
    Mute,
    Volume,
    Item(usize),
}

const ART: R = r(16.0, 16.0, 88.0, 88.0);
const COPY: R = r(340.0, 25.0, 24.0, 24.0);
const SETTINGS: R = r(368.0, 25.0, 24.0, 24.0);
/// The clickable strip is taller than the 4-unit bar.
const PROGRESS: R = r(60.0, 118.0, 284.0, 18.0);
const BACK_R: R = r(88.0, 148.0, 36.0, 36.0);
const PREVIOUS_R: R = r(130.0, 146.0, 40.0, 40.0);
const PLAY_R: R = r(178.0, 142.0, 48.0, 48.0);
const NEXT_R: R = r(234.0, 146.0, 40.0, 40.0);
const FORWARD_R: R = r(280.0, 148.0, 36.0, 36.0);
const MUTE_R: R = r(318.0, 154.0, 24.0, 24.0);
const VOLUME_R: R = r(344.0, 157.0, 44.0, 18.0);

// ---------- animation ----------

#[derive(Clone, Copy, Default)]
enum Ease {
    #[default]
    Linear,
    CubicIn,
    QuintOut,
    BackOut,
}

#[derive(Clone, Copy, Default)]
struct Anim {
    from: f32,
    to: f32,
    start: u64,
    ms: u32,
    ease: Ease,
}

impl Anim {
    const fn still(v: f32) -> Anim {
        Anim { from: v, to: v, start: 0, ms: 0, ease: Ease::Linear }
    }
    fn at(&self, now: u64) -> f32 {
        if self.done(now) {
            return self.to;
        }
        let t = now.saturating_sub(self.start) as f32 / self.ms as f32;
        let e = match self.ease {
            Ease::Linear => t,
            Ease::CubicIn => t * t * t,
            Ease::QuintOut => 1.0 - (1.0 - t).powi(5),
            Ease::BackOut => {
                let u = t - 1.0;
                1.0 + u * u * (1.6 * u + 0.6)
            }
        };
        self.from + (self.to - self.from) * e
    }
    fn done(&self, now: u64) -> bool {
        now >= self.start + self.ms as u64
    }
    fn go(&mut self, to: f32, ms: u32, ease: Ease) {
        let now = now_ms();
        *self = Anim { from: self.at(now), to, start: now, ms, ease };
    }
    fn play(&mut self, from: f32, to: f32, ms: u32, ease: Ease) {
        *self = Anim { from, to, start: now_ms(), ms, ease };
    }
}

pub const TIMER_HIDE: usize = 1;
pub const TIMER_ANIMATE: usize = 2;
const TIMER_TICK: usize = 3;
const TIMER_TOOLTIP: usize = 4;

enum Drag {
    Progress(f64),
    Volume,
}

pub struct Popup {
    pub hwnd: HWND,
    surface: Option<Surface>,
    scale: f32,
    origin: (i32, i32),
    top: bool,
    view: View,
    shown: bool,
    hiding: bool,
    seconds: f64,
    opacity: Anim,
    shift: Anim,
    zoom: Anim,
    accent: (u32, u32, Anim),
    art_fade: Anim,
    old_art: Option<Rc<Image>>,
    backdrop: Option<Rc<Image>>,
    old_backdrop: Option<Rc<Image>>,
    line_shift: Anim,
    art_zoom: Anim,
    title_fade: Anim,
    extras: Anim,
    thumb: Anim,
    scrub_badge: Anim,
    pulse: Option<(Flash, bool, u64)>,
    /// Position being previewed by a held key: (seconds, label).
    scrub: Option<(f64, String)>,
    drag: Option<Drag>,
    menu: bool,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tooltip: bool,
    notice: Option<(String, u64)>,
    shadow: Rc<Image>,
    art_shadow: Rc<Image>,
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => LRESULT(1), // set while handling mouse moves
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEWHEEL | WM_TIMER | WM_CAPTURECHANGED => {
            crate::post(crate::Event::Popup(msg, wp.0, lp.0));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(h, msg, wp, lp) },
    }
}

impl Popup {
    /// A card with no window, for rendering pictures of it.
    pub fn offscreen() -> Popup {
        Popup {
            hwnd: HWND::default(),
            surface: None,
            scale: 1.0,
            origin: (0, 0),
            top: true,
            view: View::default(),
            shown: false,
            hiding: false,
            seconds: 2.5,
            opacity: Anim::still(0.0),
            shift: Anim::still(0.0),
            zoom: Anim::still(1.0),
            accent: (art::DEFAULT_ACCENT, art::DEFAULT_ACCENT, Anim::still(1.0)),
            art_fade: Anim::still(1.0),
            old_art: None,
            backdrop: None,
            old_backdrop: None,
            line_shift: Anim::still(0.0),
            art_zoom: Anim::still(1.0),
            title_fade: Anim::still(1.0),
            extras: Anim::still(0.0),
            thumb: Anim::still(0.0),
            scrub_badge: Anim::still(0.0),
            pulse: None,
            scrub: None,
            drag: None,
            menu: false,
            hover: None,
            pressed: None,
            tooltip: false,
            notice: None,
            shadow: Rc::new(art::shadow(CARD_W as u32, CARD_H as u32, MARGIN as u32, 20.0, 30.0, 8.0, 0.5)),
            art_shadow: Rc::new(art::shadow(88, 88, 16, 12.0, 16.0, 3.0, 0.45)),
        }
    }

    pub fn new() -> Popup {
        let mut p = Self::offscreen();
        let class = wide("TempoPopup");
        unsafe {
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
            RegisterClassW(&wc);
            let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
            p.hwnd = CreateWindowExW(ex, PCWSTR(class.as_ptr()), PCWSTR(class.as_ptr()), WS_POPUP, 0, 0, 1, 1, None, None, None, None).unwrap_or_default();
        }
        p
    }

    /// On screen (including while fading out).
    pub fn visible(&self) -> bool {
        self.shown
    }

    pub fn target_key(&self) -> Option<u64> {
        self.view.target.as_ref().map(|t| t.key)
    }

    fn timer(&self, id: usize, ms: u32) {
        unsafe { SetTimer(Some(self.hwnd), id, ms, None) };
    }

    fn stop_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }

    // ---------- showing and hiding ----------

    pub fn show(&mut self, view: View, placement: Placement, follow_mouse: bool, seconds: f64) {
        self.seconds = seconds;
        let was_hidden = !self.shown;
        if was_hidden {
            let (area, scale) = system::work_area(follow_mouse);
            self.scale = scale;
            self.top = placement.is_top();
            self.origin = place(placement, (area.left, area.top, area.right, area.bottom), scale);
        }
        self.set_view(view);
        if was_hidden || self.hiding {
            let from = if was_hidden { if self.top { -14.0 } else { 14.0 } } else { self.shift.at(now_ms()) };
            self.shown = true;
            self.hiding = false;
            self.opacity.go(1.0, 170, Ease::Linear);
            self.shift.play(from, 0.0, 320, Ease::QuintOut);
            self.zoom.play(if was_hidden { 0.96 } else { self.zoom.at(now_ms()) }, 1.0, 320, Ease::QuintOut);
            self.timer(TIMER_TICK, 250);
        }
        self.animate();
        unsafe {
            if was_hidden {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    pub fn hide(&mut self) {
        if !self.shown || self.hiding {
            return;
        }
        self.hiding = true;
        self.stop_timer(TIMER_HIDE);
        self.shift.go(if self.top { -8.0 } else { 8.0 }, 200, Ease::Linear);
        self.opacity.go(0.0, 200, Ease::CubicIn);
        self.animate();
    }

    fn gone(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        for t in [TIMER_ANIMATE, TIMER_TICK, TIMER_TOOLTIP] {
            self.stop_timer(t);
        }
        (self.shown, self.hiding, self.menu, self.tooltip) = (false, false, false, false);
        (self.hover, self.pressed, self.pulse) = (None, None, None);
        // The bitmap and uploaded art are only needed while on screen.
        self.surface = None;
        self.extras = Anim::still(0.0);
        self.thumb = Anim::still(0.0);
    }

    /// Gone at once (to reappear somewhere else).
    pub fn hide_now(&mut self) {
        if self.shown {
            self.stop_timer(TIMER_HIDE);
            self.gone();
        }
    }

    pub fn restart_hide(&self, seconds: f64) {
        self.timer(TIMER_HIDE, (seconds.max(0.6) * 1000.0) as u32);
    }

    // ---------- content ----------

    pub fn set_view(&mut self, view: View) {
        let now = now_ms();
        let animated = self.shown;
        let (old, new) = (self.view.target.as_ref(), view.target.as_ref());
        if animated && old.zip(new).is_some_and(|(a, b)| a.key != b.key) {
            self.line_shift.play(-10.0, 0.0, 300, Ease::QuintOut);
        }
        if animated && old.zip(new).is_some_and(|(a, b)| (a.key, &a.title, &a.artist) != (b.key, &b.title, &b.artist)) {
            self.title_fade.play(0.2, 1.0, 360, Ease::Linear);
        }
        let old_art = old.and_then(|t| t.art.clone());
        let new_art = new.and_then(|t| t.art.clone());
        if old_art.as_ref().map(Rc::as_ptr) != new_art.as_ref().map(Rc::as_ptr) {
            self.old_art = old_art;
            self.old_backdrop = self.backdrop.take();
            // A soft, blurred look without a blur effect: a few pixels, smoothly stretched over the card.
            self.backdrop = new_art.map(|a| Rc::new(a.shrink(10, 10)));
            self.art_fade = Anim::still(1.0);
            if animated {
                self.art_fade.play(0.0, 1.0, 280, Ease::Linear);
            }
        }
        let accent = new.map_or(art::DEFAULT_ACCENT, |t| t.accent);
        if accent != self.accent.1 {
            self.accent = (self.accent_now(now), accent, Anim::still(1.0));
            if animated {
                self.accent.2.play(0.0, 1.0, 400, Ease::Linear);
            }
        }
        if self.target_key() != new.map(|t| t.key) {
            self.menu = false;
        }
        self.view = view;
        if animated {
            self.animate();
        }
    }

    fn accent_now(&self, now: u64) -> u32 {
        mix(self.accent.0, self.accent.1, self.accent.2.at(now))
    }

    /// Pulses the control that matches what just happened.
    pub fn flash(&mut self, what: Flash) {
        if what == Flash::None {
            return;
        }
        let playing = self.view.target.as_ref().is_some_and(|t| t.playing);
        self.pulse = Some((what, playing, now_ms()));
        if matches!(what, Flash::Next | Flash::Previous | Flash::Switched) {
            self.art_zoom.play(0.92, 1.0, 420, Ease::BackOut);
        }
        if what == Flash::Switched {
            self.line_shift.play(-12.0, 0.0, 320, Ease::QuintOut);
        }
        self.animate();
    }

    pub fn show_scrub(&mut self, position: f64, from: f64) {
        let delta = position - from;
        self.scrub = Some((position, format!("{}{}", if delta < 0.0 { "\u{2212}" } else { "+" }, format_time(delta.abs()))));
        self.scrub_badge.go(1.0, 100, Ease::Linear);
        self.animate();
    }

    pub fn end_scrub(&mut self) {
        self.scrub = None;
        self.scrub_badge.go(0.0, 250, Ease::Linear);
        self.animate();
    }

    /// Briefly replaces the app name with a short message.
    pub fn notice(&mut self, text: &str) {
        self.notice = Some((text.to_string(), now_ms() + 1600));
        self.line_shift.play(-8.0, 0.0, 260, Ease::QuintOut);
        self.animate();
    }

    // ---------- the right-click menu ----------

    fn menu_items(&self) -> Vec<(&'static str, String, Act, bool)> {
        let mut out = Vec::new();
        if let Some(t) = &self.view.target {
            out.push(("\u{E8A7}", format!("Open {}", t.app), Act::OpenApp, true));
            out.push(("\u{E8C8}", "Copy song name".into(), Act::CopySong, !t.title.trim().is_empty()));
            let (glyph, label) = if self.view.preferred { ("\u{E735}", format!("Stop preferring {}", t.app)) } else { ("\u{E734}", format!("Prefer {} when nothing plays", t.app)) };
            out.push((glyph, label, Act::TogglePreferred, true));
        }
        out.push(("\u{E708}", "Hide pop-ups for an hour".into(), Act::Snooze, true));
        out.push(("\u{E713}", "Tempo settings".into(), Act::Settings, true));
        out
    }

    fn menu_rect(&self) -> R {
        let items = self.menu_items();
        let widest = items.iter().map(|i| measure(&i.1, text(12.5), 1000.0, false).0).fold(0.0, f32::max);
        let w = (widest + 26.0 + 20.0 + 8.0).clamp(208.0, CARD_W - 20.0);
        r(CARD_W - 10.0 - w, 10.0, w, items.len() as f32 * 30.0 + 8.0)
    }

    fn item_rect(menu: R, i: usize) -> R {
        r(menu.x + 4.0, menu.y + 4.0 + i as f32 * 30.0, menu.w - 8.0, 30.0)
    }

    // ---------- hit testing ----------

    fn source_rect(i: usize, count: usize) -> R {
        r(336.0 - 24.0 - (count - 1 - i) as f32 * 26.0, 25.0, 24.0, 24.0)
    }

    fn enabled(&self, hit: Hit) -> bool {
        let Some(t) = &self.view.target else { return matches!(hit, Hit::Card | Hit::Settings | Hit::Item(_)) };
        match hit {
            Hit::Previous => t.can_previous,
            Hit::Next => t.can_next,
            Hit::Back | Hit::Forward | Hit::Progress => t.can_seek && t.timeline.duration > 0.0,
            Hit::Mute | Hit::Volume => self.view.volume.is_some(),
            Hit::Item(i) => self.menu_items().get(i).is_some_and(|m| m.3),
            _ => true,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !r(0.0, 0.0, CARD_W, CARD_H).contains(x, y) {
            return None;
        }
        if self.menu {
            let menu = self.menu_rect();
            return Some((0..self.menu_items().len()).find(|i| Self::item_rect(menu, *i).contains(x, y)).map_or(Hit::Card, Hit::Item));
        }
        let n = self.view.sources.len();
        if n > 1 {
            if let Some(i) = (0..n).find(|i| Self::source_rect(*i, n).contains(x, y)) {
                return Some(Hit::Source(self.view.sources[i].key));
            }
        }
        let fixed = [
            (Hit::Art, ART),
            (Hit::Copy, COPY),
            (Hit::Settings, SETTINGS),
            (Hit::Progress, PROGRESS),
            (Hit::Back, BACK_R),
            (Hit::Previous, PREVIOUS_R),
            (Hit::Play, PLAY_R),
            (Hit::Next, NEXT_R),
            (Hit::Forward, FORWARD_R),
            (Hit::Mute, MUTE_R),
            (Hit::Volume, VOLUME_R),
        ];
        Some(fixed.iter().find(|(h, rect)| rect.contains(x, y) && self.enabled(*h)).map_or(Hit::Card, |f| f.0))
    }

    fn position_at(&self, x: f32) -> f64 {
        let d = self.view.target.as_ref().map_or(0.0, |t| t.timeline.duration);
        d * ((x - PROGRESS.x) / PROGRESS.w).clamp(0.0, 1.0) as f64
    }

    // ---------- input ----------

    /// Handles one of the window's messages and returns what the user asked for.
    pub fn message(&mut self, msg: u32, wp: usize, lp: isize) -> Vec<Act> {
        let mut acts = Vec::new();
        if !self.shown {
            return acts;
        }
        let x = (lp & 0xFFFF) as i16 as f32 / self.scale - MARGIN;
        let y = ((lp >> 16) & 0xFFFF) as i16 as f32 / self.scale - MARGIN;
        match msg {
            WM_TIMER => self.on_timer(wp),
            WM_MOUSEMOVE => {
                let over = self.hit(x, y);
                if over != self.hover {
                    self.tooltip = false;
                    self.stop_timer(TIMER_TOOLTIP);
                    if over.is_some_and(|h| h != Hit::Card) {
                        self.timer(TIMER_TOOLTIP, 600);
                    }
                    match (self.hover.is_some(), over.is_some()) {
                        (false, true) => {
                            self.extras.go(1.0, 150, Ease::Linear);
                            let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                            unsafe {
                                let _ = TrackMouseEvent(&mut t);
                            }
                        }
                        (true, false) => self.left(),
                        _ => {}
                    }
                    let on_bar = over == Some(Hit::Progress);
                    if on_bar != (self.hover == Some(Hit::Progress)) && self.drag.is_none() {
                        self.thumb.go(if on_bar { 1.0 } else { 0.0 }, if on_bar { 120 } else { 200 }, Ease::Linear);
                    }
                    self.hover = over;
                }
                match self.drag {
                    Some(Drag::Progress(_)) => {
                        self.drag = Some(Drag::Progress(self.position_at(x)));
                        self.restart_hide(self.seconds);
                    }
                    Some(Drag::Volume) => acts.extend(self.volume_to((x - VOLUME_R.x) / VOLUME_R.w)),
                    None => {}
                }
                unsafe {
                    let clickable = self.hover.is_some_and(|h| h != Hit::Card);
                    SetCursor(LoadCursorW(None, if clickable { IDC_HAND } else { IDC_ARROW }).ok());
                }
                self.animate();
            }
            WM_MOUSELEAVE => {
                if self.hover.is_some() && self.drag.is_none() {
                    self.left();
                    self.hover = None;
                    self.animate();
                }
            }
            WM_LBUTTONDOWN => {
                let over = self.hit(x, y);
                self.pressed = over;
                self.tooltip = false;
                match over {
                    Some(Hit::Progress) => {
                        self.drag = Some(Drag::Progress(self.position_at(x)));
                        unsafe { SetCapture(self.hwnd) };
                    }
                    Some(Hit::Volume) => {
                        self.drag = Some(Drag::Volume);
                        unsafe { SetCapture(self.hwnd) };
                        acts.extend(self.volume_to((x - VOLUME_R.x) / VOLUME_R.w));
                    }
                    _ => {}
                }
                self.animate();
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                if let Some(drag) = self.drag.take() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    if let Drag::Progress(_) = drag {
                        let position = self.position_at(x);
                        if let Some(t) = self.view.target.as_mut() {
                            (t.timeline.position, t.timeline.at_ms) = (position, now_ms());
                        }
                        acts.push(Act::Seek(position));
                    }
                } else if self.menu {
                    match self.hit(x, y) {
                        Some(Hit::Item(i)) if self.enabled(Hit::Item(i)) => {
                            let act = self.menu_items()[i].2;
                            self.close_menu();
                            acts.push(act);
                        }
                        Some(Hit::Card) => self.close_menu(),
                        _ => {}
                    }
                } else if pressed.is_some() && pressed == self.hit(x, y) {
                    let skip = self.view.skip as f64;
                    acts.extend(match pressed.unwrap() {
                        Hit::Art => Some(Act::OpenApp),
                        Hit::Settings => Some(Act::Settings),
                        Hit::Copy => Some(Act::CopySong),
                        Hit::Source(k) => Some(Act::Pin(k)),
                        Hit::Back => Some(Act::Skip(-skip)),
                        Hit::Previous => Some(Act::Previous),
                        Hit::Play => Some(Act::PlayPause),
                        Hit::Next => Some(Act::Next),
                        Hit::Forward => Some(Act::Skip(skip)),
                        Hit::Mute => Some(Act::ToggleMute),
                        _ => None,
                    });
                }
                self.animate();
            }
            WM_CAPTURECHANGED => {
                self.drag = None;
            }
            WM_RBUTTONUP => {
                if self.hit(x, y).is_some() {
                    self.menu = true;
                    self.tooltip = false;
                    self.stop_timer(TIMER_HIDE);
                    self.animate();
                }
            }
            WM_MOUSEWHEEL => {
                if let Some((level, _)) = self.view.volume {
                    let up = (wp >> 16) as i16 > 0;
                    acts.extend(self.volume_to(level + if up { 0.04 } else { -0.04 }));
                    self.restart_hide(self.seconds);
                    self.animate();
                }
            }
            _ => {}
        }
        acts
    }

    fn volume_to(&mut self, level: f32) -> Option<Act> {
        self.view.volume?;
        let level = level.clamp(0.0, 1.0);
        self.view.volume = Some((level, false));
        Some(Act::Volume(level))
    }

    /// The mouse left the card.
    fn left(&mut self) {
        self.extras.go(0.0, 250, Ease::Linear);
        self.thumb.go(0.0, 200, Ease::Linear);
        self.tooltip = false;
        self.stop_timer(TIMER_TOOLTIP);
        self.menu = false;
        self.restart_hide(self.seconds.min(1.5));
    }

    fn close_menu(&mut self) {
        self.menu = false;
        self.restart_hide(self.seconds.min(1.5));
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_HIDE => {
                if self.hover.is_some() || self.drag.is_some() || self.menu {
                    self.restart_hide(1.2);
                } else {
                    self.stop_timer(TIMER_HIDE);
                    self.hide();
                }
            }
            TIMER_ANIMATE => self.animate(),
            TIMER_TICK => self.render(),
            TIMER_TOOLTIP => {
                self.stop_timer(TIMER_TOOLTIP);
                self.tooltip = true;
                self.render();
            }
            _ => {}
        }
    }

    fn busy(&self, now: u64) -> bool {
        let anims = [self.opacity, self.shift, self.zoom, self.accent.2, self.art_fade, self.line_shift, self.art_zoom, self.title_fade, self.extras, self.thumb, self.scrub_badge];
        anims.iter().any(|a| !a.done(now)) || self.pulse.is_some_and(|p| now < p.2 + 850) || self.notice.as_ref().is_some_and(|n| now < n.1)
    }

    /// Draws now, and keeps a fast timer running only while something moves.
    fn animate(&mut self) {
        if !self.shown {
            return;
        }
        let now = now_ms();
        if self.notice.as_ref().is_some_and(|n| now >= n.1) {
            self.notice = None;
        }
        if self.hiding && self.opacity.done(now) {
            self.gone();
            return;
        }
        if self.busy(now) {
            self.timer(TIMER_ANIMATE, 15);
        } else {
            self.stop_timer(TIMER_ANIMATE);
        }
        self.render();
    }

    // ---------- drawing ----------

    fn render(&mut self) {
        if !self.shown {
            return;
        }
        let (w, h) = size(self.scale);
        if self.surface.as_ref().is_none_or(|s| s.w != w || s.h != h) {
            self.surface = Surface::new(w, h);
        }
        let Some(surface) = &self.surface else { return };
        let now = now_ms();
        self.draw(surface, self.scale, now);
        let position = POINT { x: self.origin.0, y: self.origin.1 + (self.shift.at(now) * self.scale).round() as i32 };
        let size = SIZE { cx: w, cy: h };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: (self.opacity.at(now).clamp(0.0, 1.0) * 255.0) as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe {
            let _ = UpdateLayeredWindow(self.hwnd, None, Some(&position), Some(&size), Some(surface.dc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }

    /// Draws the card as it looks at `now`.
    pub fn draw(&self, s: &Surface, scale: f32, now: u64) {
        let accent = self.accent_now(now);
        let m = MARGIN;
        // card coordinates to canvas coordinates
        let c = |rect: R| r(rect.x + m, rect.y + m, rect.w, rect.h);
        let card = c(r(0.0, 0.0, CARD_W, CARD_H));
        let hover = |h: Hit| self.hover == Some(h) && self.enabled(h);
        s.begin(scale, 0);
        let (cx, cy) = card.center();
        s.zoom(self.zoom.at(now), cx, cy);
        s.image(&self.shadow, r(0.0, 0.0, CARD_W + m * 2.0, CARD_H + m * 2.0), 1.0);
        s.fill(card, 20.0, 0xF2141519);
        s.push_clip(card, 20.0);

        let fade_in = self.art_fade.at(now);
        let blur = r(cx - CARD_W * 0.675, cy - CARD_H * 0.8, CARD_W * 1.35, CARD_H * 1.6);
        if let Some(b) = &self.old_backdrop {
            s.image(b, blur, 0.6 * (1.0 - fade_in));
        }
        if let Some(b) = &self.backdrop {
            s.image(b, blur, 0.6 * fade_in);
        }
        s.gradient(card, 0.0, &[(0.0, 0xA6111216), (0.65, 0xE0111216), (1.0, 0xF0111216)]);
        s.glow(c(r(-90.0, -90.0, 260.0, 200.0)), fade(accent, 0.30));

        let track = self.view.target.as_ref();
        self.draw_art(s, c(ART), accent, now);

        // app line, other apps, copy and settings
        let notice = self.notice.as_ref().filter(|n| now < n.1);
        let shift = self.line_shift.at(now);
        let sources = if self.view.sources.len() > 1 { self.view.sources.len() } else { 0 };
        let line_end = if sources > 0 { Self::source_rect(0, sources).x - 6.0 } else { 334.0 };
        let mut x = 118.0 + shift;
        if let Some(icon) = track.and_then(|t| t.icon.as_ref()) {
            s.image(icon, c(r(x, 30.0, 14.0, 14.0)), 1.0);
            x += 20.0;
        }
        let name = notice.map_or_else(|| track.map_or("Tempo".to_string(), |t| t.app.clone()), |n| n.0.clone());
        let room = if notice.is_some() { line_end - x } else { (line_end - x - 12.0).min(150.0) };
        let name_w = measure(&name, bold(11.5), room, false).0;
        s.text(&name, c(r(x, 28.0, name_w + 1.0, 18.0)), bold(11.5), if notice.is_some() { accent } else { MUTED }, Align::Left);
        if track.is_some_and(|t| t.playing) {
            s.circle(m + x + name_w + 9.5, m + 38.0, 2.5, accent);
        }
        for (i, src) in self.view.sources.iter().enumerate().take(sources) {
            let rect = c(Self::source_rect(i, sources));
            let current = track.is_some_and(|t| t.key == src.key);
            let opacity = if current { 1.0 } else { 0.55 };
            if current {
                s.fill(rect, 12.0, 0x2EFFFFFF);
            }
            if hover(Hit::Source(src.key)) {
                s.fill(rect, 12.0, 0x1AFFFFFF);
            }
            match &src.icon {
                Some(icon) => s.image(icon, rect.inset(5.0), opacity),
                None => s.text(&src.name.chars().take(1).collect::<String>(), rect, Font { face: Face::Text, size: 10.0, weight: 700 }, fade(FG, opacity), Align::Center),
            }
            if src.playing {
                s.circle(rect.x + 21.5, rect.y + 21.5, 2.5, fade(accent, opacity));
            }
        }
        let extras = self.extras.at(now);
        for (hit, rect, glyph) in [(Hit::Copy, COPY, "\u{E8C8}"), (Hit::Settings, SETTINGS, "\u{E713}")] {
            if hover(hit) {
                s.fill(c(rect), 12.0, fade(0x1AFFFFFF, extras));
            }
            s.glyph(glyph, c(rect), 12.0, fade(MUTED, extras));
        }

        let title_fade = self.title_fade.at(now);
        let (title, artist) = match track {
            None => ("Nothing to control".to_string(), "Open Spotify, a video or a player first".to_string()),
            Some(t) => (
                if t.title.trim().is_empty() { t.app.clone() } else { t.title.clone() },
                if t.artist.trim().is_empty() { if t.playing { "Playing" } else { "Paused" }.to_string() } else { t.artist.clone() },
            ),
        };
        s.text(&title, c(r(118.0, 51.0, 270.0, 23.0)), display(17.0), fade(FG, title_fade), Align::Left);
        s.text(&artist, c(r(118.0, 76.0, 270.0, 18.0)), text(13.0), fade(MUTED, title_fade), Align::Left);

        self.draw_progress(s, accent, now);

        // transport
        let playing = track.is_some_and(|t| t.playing);
        let button = |hit: Hit, rect: R, draw: &dyn Fn(f32, f32, f32)| {
            let rect = c(rect);
            let (bx, by) = rect.center();
            let opacity = if self.enabled(hit) { 1.0 } else { 0.32 };
            let mut k = if self.pressed == Some(hit) && self.enabled(hit) { 0.9 } else { 1.0 };
            if let Some((what, _, at)) = self.pulse {
                let mine = match what {
                    Flash::PlayPause | Flash::Stop => Hit::Play,
                    Flash::Next => Hit::Next,
                    Flash::Previous => Hit::Previous,
                    _ => Hit::Card,
                };
                if mine == hit {
                    k *= Anim { from: 0.86, to: 1.0, start: at, ms: 380, ease: Ease::BackOut }.at(now);
                }
            }
            s.transform(k, bx, by);
            if hit == Hit::Play {
                s.circle(bx, by, rect.w / 2.0, fade(accent, opacity));
            }
            if hover(hit) {
                s.circle(bx, by, rect.w / 2.0, if self.pressed == Some(hit) { 0x2EFFFFFF } else { 0x1AFFFFFF });
            }
            draw(bx, by, opacity);
            s.transform(1.0, 0.0, 0.0);
        };
        let skip = self.view.skip.to_string();
        for (hit, rect, mirror) in [(Hit::Back, BACK_R, false), (Hit::Forward, FORWARD_R, true)] {
            button(hit, rect, &|x, y, o| {
                s.path(BACK, r(x - 11.0, y - 11.0, 22.0, 22.0), fade(FG, o), mirror, None);
                s.text(&skip, r(x - 11.0, y - 6.0, 22.0, 15.0), Font { face: Face::Text, size: 7.5, weight: 700 }, fade(FG, o), Align::Center);
            });
        }
        for (hit, rect, mirror) in [(Hit::Previous, PREVIOUS_R, true), (Hit::Next, NEXT_R, false)] {
            button(hit, rect, &|x, y, o| s.path(NEXT, r(x - 10.0, y - 10.0, 20.0, 20.0), fade(FG, o), mirror, None));
        }
        if let Some((Flash::PlayPause | Flash::Stop, _, at)) = self.pulse {
            if now < at + 500 {
                let t = (now - at) as f32 / 500.0;
                let (bx, by) = c(PLAY_R).center();
                s.ring(bx, by, 23.0 * (1.0 + 0.45 * t), fade(accent, 0.8 * (1.0 - t)), 2.0);
            }
        }
        button(Hit::Play, PLAY_R, &|x, y, o| {
            let nudge = if playing { 0.0 } else { 0.5 };
            s.path(if playing { PAUSE } else { PLAY }, r(x - 11.0 + nudge, y - 11.0, 22.0, 22.0), fade(DARK, o), false, None);
        });

        if let Some((level, muted)) = self.view.volume {
            if hover(Hit::Mute) {
                s.fill(c(MUTE_R), 12.0, 0x1AFFFFFF);
            }
            let (x, y) = c(MUTE_R).center();
            let icon = r(x - 7.5, y - 7.5, 15.0, 15.0);
            s.path(SPEAKER, icon, MUTED, false, None);
            s.path(if muted || level < 0.005 { MUTE_X } else { WAVES }, icon, MUTED, false, Some(1.8));
            let v = c(VOLUME_R);
            let o = if muted { 0.4 } else { 1.0 };
            let bar = r(v.x, v.y + 7.0, v.w, 4.0);
            s.fill(bar, 2.0, fade(0x26FFFFFF, o));
            s.fill(r(bar.x, bar.y, bar.w * level, bar.h), 2.0, fade(accent, o));
            s.circle(bar.x + 6.0 + (bar.w - 12.0) * level, bar.y + 2.0, 6.0, fade(FG, o));
        }

        if self.menu {
            s.fill(card, 20.0, 0x59000000);
            let menu = self.menu_rect();
            s.fill(c(r(menu.x, menu.y + 3.0, menu.w, menu.h)), 12.0, 0x40000000);
            s.fill(c(menu), 10.0, 0xF51C1D23);
            s.border(c(menu), 10.0, 0x2EFFFFFF, 1.0);
            for (i, (glyph, label, _, enabled)) in self.menu_items().iter().enumerate() {
                let rect = c(Self::item_rect(menu, i));
                let o = if *enabled { 1.0 } else { 0.4 };
                if hover(Hit::Item(i)) {
                    s.fill(rect, 6.0, if self.pressed == Some(Hit::Item(i)) { 0x26FFFFFF } else { 0x1AFFFFFF });
                }
                s.glyph(glyph, r(rect.x + 8.0, rect.y, 18.0, rect.h), 13.0, fade(MUTED, o));
                s.text(label, r(rect.x + 36.0, rect.y, rect.w - 40.0, rect.h), text(12.5), fade(FG, o), Align::Left);
            }
        }
        if self.tooltip {
            self.draw_tooltip(s);
        }

        s.pop_clip();
        s.border(card, 20.0, 0x1CFFFFFF, 1.0);
        s.end();
    }

    fn draw_art(&self, s: &Surface, rect: R, accent: u32, now: u64) {
        let (x, y) = rect.center();
        s.transform(self.art_zoom.at(now), x, y);
        s.image(&self.art_shadow, r(rect.x - 16.0, rect.y - 16.0, rect.w + 32.0, rect.h + 32.0), 1.0);
        let t = self.art_fade.at(now);
        let art = self.view.target.as_ref().and_then(|t| t.art.as_ref());
        s.fill(rect, 12.0, 0xFF1F2026);
        if art.is_none() || t < 1.0 {
            s.glyph("\u{E8D6}", rect, 30.0, fade(accent, 0.85));
        }
        if let Some(old) = self.old_art.as_ref().filter(|_| t < 1.0) {
            s.cover(old, rect, 12.0, 1.0 - t);
        }
        if let Some(a) = art {
            s.cover(a, rect, 12.0, t);
        }
        // A big glyph pops over the art on a key press: what just happened.
        if let Some((what, was_playing, at)) = self.pulse {
            let age = now.saturating_sub(at) as f32;
            let alpha = if age < 90.0 { age / 90.0 } else if age < 550.0 { 1.0 } else { ((850.0 - age) / 300.0).max(0.0) };
            let playing = self.view.target.as_ref().map_or(was_playing, |t| t.playing);
            let glyph = match what {
                Flash::PlayPause => Some((if playing { PLAY } else { PAUSE }, false)),
                Flash::Next => Some((NEXT, false)),
                Flash::Previous => Some((NEXT, true)),
                Flash::Stop => Some((PAUSE, false)),
                _ => None,
            };
            if let Some((g, mirror)) = glyph.filter(|_| alpha > 0.0) {
                s.fill(rect, 12.0, fade(0x73000000, alpha));
                s.path(g, r(x - 17.0, y - 17.0, 34.0, 34.0), fade(0xFFFFFFFF, alpha), mirror, None);
            }
        }
        s.transform(1.0, 0.0, 0.0);
    }

    fn draw_progress(&self, s: &Surface, accent: u32, now: u64) {
        let Some(t) = &self.view.target else { return };
        let m = MARGIN;
        let duration = t.timeline.duration;
        let preview = match &self.drag {
            Some(Drag::Progress(p)) => Some((*p, format_time(*p), 1.0)),
            _ => self.scrub.as_ref().map(|(p, label)| (*p, label.clone(), self.scrub_badge.at(now))),
        };
        let position = preview.as_ref().map_or_else(|| t.timeline.now(t.playing, now_ms()), |p| p.0);
        let fraction = if duration > 0.0 { (position / duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let o = if duration > 0.0 { 1.0 } else { 0.35 };
        let times = if preview.is_some() { accent } else { MUTED };
        if duration > 0.0 {
            s.text(&format_time(position), r(m + 16.0, m + 118.0, 44.0, 18.0), text(11.0), times, Align::Left);
        }
        let end = if duration > 0.0 { format_time(duration) } else if t.playing { "Live".to_string() } else { String::new() };
        s.text(&end, r(m + 344.0, m + 118.0, 44.0, 18.0), text(11.0), MUTED, Align::Right);
        let bar = r(m + PROGRESS.x, m + 125.0, PROGRESS.w, 4.0);
        s.fill(bar, 2.0, fade(0x26FFFFFF, o));
        s.fill(r(bar.x, bar.y, bar.w * fraction, bar.h), 2.0, fade(accent, o));
        let thumb = if preview.is_some() { 1.0 } else { self.thumb.at(now) };
        let at = bar.x + bar.w * fraction;
        s.circle(at, bar.y + 2.0, 6.0, fade(FG, thumb));
        if let Some((_, label, alpha)) = preview {
            let w = measure(&label, bold(11.0), 200.0, false).0 + 12.0;
            let x = (at - w / 2.0).clamp(bar.x, (bar.x + bar.w - w).max(bar.x));
            let badge = r(x, bar.y - 24.0, w, 17.0);
            s.fill(badge, 6.0, fade(accent, alpha));
            s.text(&label, badge, bold(11.0), fade(DARK, alpha), Align::Center);
        }
    }

    fn draw_tooltip(&self, s: &Surface) {
        let Some(hit) = self.hover else { return };
        let app = self.view.target.as_ref().map_or("", |t| t.app.as_str());
        let n = self.view.sources.len();
        let (label, rect) = match hit {
            Hit::Art => ("Open the app".to_string(), ART),
            Hit::Copy => ("Copy song name".to_string(), COPY),
            Hit::Settings => ("Tempo settings".to_string(), SETTINGS),
            Hit::Source(k) => {
                let Some((i, src)) = self.view.sources.iter().enumerate().find(|(_, x)| x.key == k) else { return };
                let verb = if self.target_key() == Some(k) { "Controlling " } else { "Switch to " };
                let title = if src.title.is_empty() { String::new() } else { format!(" — {}", src.title) };
                (format!("{verb}{}{title}", src.name), Self::source_rect(i, n))
            }
            Hit::Back => (format!("Back {} seconds", self.view.skip), BACK_R),
            Hit::Forward => (format!("Forward {} seconds", self.view.skip), FORWARD_R),
            Hit::Previous => ("Previous".to_string(), PREVIOUS_R),
            Hit::Play => ("Play / pause".to_string(), PLAY_R),
            Hit::Next => ("Next".to_string(), NEXT_R),
            Hit::Mute => (format!("{} {app}", if self.view.volume.is_some_and(|v| v.1) { "Unmute" } else { "Mute" }), MUTE_R),
            Hit::Volume => ("App volume (or scroll anywhere on the card)".to_string(), VOLUME_R),
            _ => return,
        };
        let w = (measure(&label, text(12.0), CARD_W - 32.0, false).0 + 16.0).min(CARD_W - 12.0);
        let x = (rect.center().0 - w / 2.0).clamp(6.0, CARD_W - w - 6.0);
        // above the control, or below it near the top of the card
        let y = if rect.y > 40.0 { rect.y - 28.0 } else { rect.y + rect.h + 4.0 };
        let tip = r(MARGIN + x, MARGIN + y, w, 24.0);
        s.fill(tip, 6.0, 0xFF202127);
        s.border(tip, 6.0, 0x26FFFFFF, 1.0);
        s.text(&label, tip.inset(1.0), text(12.0), FG, Align::Center);
    }

    /// For pictures: the card at rest, optionally with the pointer over `hover_extras`.
    pub fn pose(&mut self, view: View, menu: bool, scrub: Option<(f64, f64)>) {
        self.set_view(view);
        self.shown = true;
        self.opacity = Anim::still(1.0);
        self.extras = Anim::still(1.0);
        self.menu = menu;
        if let Some((position, from)) = scrub {
            self.show_scrub_still(position, from);
        }
        self.shown = false;
    }

    fn show_scrub_still(&mut self, position: f64, from: f64) {
        let delta = position - from;
        self.scrub = Some((position, format!("{}{}", if delta < 0.0 { "\u{2212}" } else { "+" }, format_time(delta.abs()))));
        self.scrub_badge = Anim::still(1.0);
    }
}

/// Canvas size in pixels at a screen scale.
pub fn size(scale: f32) -> (i32, i32) {
    (((CARD_W + MARGIN * 2.0) * scale).round() as i32, ((CARD_H + MARGIN * 2.0) * scale).round() as i32)
}

/// Top-left corner of the window for the chosen corner or edge of a work area (physical pixels).
pub fn place(placement: Placement, area: (i32, i32, i32, i32), scale: f32) -> (i32, i32) {
    let (left, top, right, bottom) = area;
    let (w, h) = size(scale);
    let margin = (MARGIN * scale) as i32; // transparent shadow room around the card
    let gap = (14.0 * scale) as i32; // visible gap between card and screen edge
    let (l, r) = (left + gap - margin, right - w - gap + margin);
    let (t, b) = (top + gap - margin, bottom - h - gap + margin);
    let cx = left + (right - left - w) / 2;
    match placement {
        Placement::TopLeft => (l, t),
        Placement::TopCenter => (cx, t + (10.0 * scale) as i32),
        Placement::TopRight => (r, t),
        Placement::BottomLeft => (l, b),
        Placement::BottomCenter => (cx, b),
        Placement::BottomRight => (r, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_keeps_the_card_on_screen() {
        let area = (0, 0, 1920, 1040);
        for p in [Placement::TopLeft, Placement::TopCenter, Placement::TopRight, Placement::BottomLeft, Placement::BottomCenter, Placement::BottomRight] {
            let (x, y) = place(p, area, 1.5);
            let m = (MARGIN * 1.5) as i32;
            assert!(x + m >= 0 && y + m >= 0, "{p:?}");
            assert!(x + size(1.5).0 - m <= 1920 && y + size(1.5).1 - m <= 1040, "{p:?}");
        }
        assert_eq!(place(Placement::TopLeft, area, 1.0), (-14, -14));
    }

    #[test]
    fn hit_testing_follows_what_is_enabled() {
        let mut p = Popup::offscreen();
        assert_eq!(p.hit(200.0, 160.0), Some(Hit::Card), "nothing to control");
        let track = Track { key: 1, app: "A".into(), can_next: true, ..Default::default() };
        p.pose(View { target: Some(track), skip: 10, ..Default::default() }, false, None);
        assert_eq!(p.hit(200.0, 160.0), Some(Hit::Play));
        assert_eq!(p.hit(254.0, 166.0), Some(Hit::Next));
        assert_eq!(p.hit(150.0, 166.0), Some(Hit::Card), "previous is disabled");
        assert_eq!(p.hit(-1.0, 5.0), None);
        p.menu = true;
        assert_eq!(p.hit(380.0, 30.0), Some(Hit::Item(0)));
    }
}
