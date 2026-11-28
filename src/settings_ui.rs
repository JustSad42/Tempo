//! The settings window: one scrolling page drawn like the pop-up. Every control is reachable with
//! the mouse or the keyboard (Tab, Shift+Tab, Space/Enter, arrow keys, Esc).

use crate::art::Image;
use crate::config::{Placement, Settings};
use crate::gfx::*;
use crate::system::{self, wide};
use std::cell::Cell;
use std::rc::Rc;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{PCWSTR, w};

const BG: u32 = 0xFF16171B;
const FG: u32 = 0xFFF2F3F7;
const MUTED: u32 = 0xFFA3A6B4;
const FAINT: u32 = 0xFF6E7180;
const CARD: u32 = 0x0DFFFFFF;
const LINE: u32 = 0x14FFFFFF;
const ACCENT: u32 = 0xFF8AA2FF;
const TIMER_TOOLTIP: usize = 1;

pub struct AppRow {
    pub id: String,
    pub name: String,
    pub icon: Option<Rc<Image>>,
    pub sub: String,
    pub preferred: bool,
    pub enabled: bool,
}

pub struct TrackRow {
    pub icon: Option<Rc<Image>>,
    pub title: String,
    pub sub: String,
}

#[derive(Default)]
pub struct Model {
    pub settings: Settings,
    pub starts_with_windows: bool,
    pub welcome: bool,
    pub status_art: Option<Rc<Image>>,
    pub status_title: String,
    pub status_sub: String,
    pub apps: Vec<AppRow>,
    pub history: Vec<TrackRow>,
    pub stats: Vec<(String, &'static str)>,
    pub stats_since: String,
    pub update_status: String,
    pub logo: Option<Rc<Image>>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Toggle {
    ShowPopup,
    Fullscreen,
    Scrub,
    HoldSwitch,
    DoubleTap,
    AlwaysCapture,
    PassThrough,
    AutoUpdate,
    Startup,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Set(Toggle, bool),
    Place(Placement),
    FollowMouse(bool),
    Duration(f64),
    Skip(u32),
    Prefer(usize),
    AppEnabled(usize, bool),
    ClearHistory,
    YouTube(usize),
    Spotify(usize),
    Copy(usize),
    CheckUpdates,
    Preview,
    Quit,
    Close,
}

#[derive(Clone, Copy)]
enum Kind {
    Button(Action),
    /// The pop-up duration slider.
    Slider(f64),
}

struct Control {
    rect: R,
    kind: Kind,
    tip: &'static str,
}

enum Drag {
    Slider(R),
    /// The scrollbar: where on the thumb it was grabbed.
    Scroll(f32),
}

pub struct Window {
    pub hwnd: HWND,
    surface: Option<Surface>,
    scroll: f32,
    content: f32,
    controls: Vec<Control>,
    hover: Option<usize>,
    pressed: Option<usize>,
    /// Keyboard focus; the ring only shows once the keyboard is used.
    focus: Option<usize>,
    drag: Option<Drag>,
    tooltip: bool,
    pub model: Model,
}

thread_local!(static PAINT: Cell<(isize, i32, i32)> = const { Cell::new((0, 0, 0)) });

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(h, &mut ps);
                let (src, w, h_) = PAINT.get();
                if src != 0 {
                    let _ = BitBlt(dc, 0, 0, w, h_, Some(HDC(src as _)), 0, 0, SRCCOPY);
                }
                let _ = EndPaint(h, &ps);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let k = GetDpiForWindow(h) as f32 / 96.0;
                let m = &mut *(lp.0 as *mut MINMAXINFO);
                m.ptMinTrackSize = POINT { x: (480.0 * k) as i32, y: (480.0 * k) as i32 };
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(h, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_SIZE | WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEWHEEL | WM_KEYDOWN | WM_TIMER | WM_CAPTURECHANGED | WM_CLOSE => {
                crate::post(crate::Event::Settings(msg, wp.0, lp.0));
                // Resizing runs its own message loop: handle it there and then.
                crate::pump();
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }
}

impl Window {
    pub fn offscreen(model: Model) -> Window {
        Window { hwnd: HWND::default(), surface: None, scroll: 0.0, content: 0.0, controls: Vec::new(), hover: None, pressed: None, focus: None, drag: None, tooltip: false, model }
    }

    pub fn open(model: Model) -> Window {
        let mut me = Self::offscreen(model);
        let class = wide("TempoSettings");
        unsafe {
            let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                lpszClassName: PCWSTR(class.as_ptr()),
                hInstance: module.unwrap_or_default(),
                hIcon: LoadIconW(module, PCWSTR(1 as _)).unwrap_or_default(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let (area, k) = system::work_area(true);
            let mut rect = RECT { left: 0, top: 0, right: (560.0 * k) as i32, bottom: (760.0 * k) as i32 };
            let _ = AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), (96.0 * k) as u32);
            let (w, h) = (rect.right - rect.left, (rect.bottom - rect.top).min(area.bottom - area.top));
            let (x, y) = (area.left + (area.right - area.left - w) / 2, area.top + (area.bottom - area.top - h) / 2);
            let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), PCWSTR(class.as_ptr()), w!("Tempo"), WS_OVERLAPPEDWINDOW, x, y, w, h, None, None, module, None) else { return me };
            me.hwnd = hwnd;
            // dark title bar, rounded corners
            for (attribute, value) in [(20, 1i32), (33, 2)] {
                let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attribute), &value as *const _ as _, 4);
            }
        }
        me.render();
        unsafe {
            let _ = ShowWindow(me.hwnd, SW_SHOW);
        }
        me.activate();
        me
    }

    pub fn activate(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn close(self) {
        PAINT.set((0, 0, 0));
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn update(&mut self, model: Model) {
        self.model = model;
        self.render();
    }

    fn client(&self) -> (i32, i32, f32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        let k = unsafe { GetDpiForWindow(self.hwnd) } as f32 / 96.0;
        (rect.right, rect.bottom, k.max(1.0))
    }

    pub fn render(&mut self) {
        let (w, h, k) = self.client();
        if w <= 0 || h <= 0 {
            return;
        }
        if self.surface.as_ref().is_none_or(|s| s.w != w || s.h != h) {
            self.surface = Surface::new(w, h);
        }
        let Some(surface) = self.surface.take() else { return };
        self.draw(&surface, w as f32 / k, h as f32 / k, k);
        PAINT.set((surface.dc.0 as isize, w, h));
        self.surface = Some(surface);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    // ---------- drawing ----------

    pub fn draw(&mut self, s: &Surface, width: f32, height: f32, scale: f32) {
        self.scroll = self.scroll.clamp(0.0, (self.content - height).max(0.0));
        s.begin(scale, BG);
        self.controls.clear();
        let w = (width - 52.0).min(640.0);
        let x = ((width - w) / 2.0).max(24.0);
        let top = -self.scroll;
        let mut page = Page { s, x, w, y: top + 24.0, controls: &mut self.controls, hover: self.hover, pressed: self.pressed };
        page.contents(&self.model);
        self.content = page.y + 28.0 - top;

        if let Some(c) = self.focus.and_then(|i| self.controls.get(i)) {
            s.border(c.rect.inset(-3.0), 9.0, ACCENT, 2.0);
        }
        if self.tooltip {
            if let Some(c) = self.hover.and_then(|i| self.controls.get(i)).filter(|c| !c.tip.is_empty()) {
                let tw = measure(c.tip, text(12.0), width - 24.0, false).0 + 18.0;
                let tip = r((c.rect.center().0 - tw / 2.0).clamp(6.0, (width - tw - 6.0).max(6.0)), if c.rect.y > 36.0 { c.rect.y - 28.0 } else { c.rect.y + c.rect.h + 4.0 }, tw, 24.0);
                s.fill(tip, 6.0, 0xFF202127);
                s.border(tip, 6.0, 0x26FFFFFF, 1.0);
                s.text(c.tip, tip.inset(1.0), text(12.0), FG, Align::Center);
            }
        }
        if let Some(thumb) = self.thumb(width, height) {
            s.fill(thumb, 3.0, 0x33FFFFFF);
        }
        s.end();
    }

    fn thumb(&self, width: f32, height: f32) -> Option<R> {
        (self.content > height).then(|| {
            let len = (height * height / self.content).max(32.0);
            let at = self.scroll / (self.content - height) * (height - len);
            r(width - 9.0, at, 6.0, len)
        })
    }

    // ---------- input ----------

    fn at(&self, x: f32, y: f32) -> Option<usize> {
        self.controls.iter().rposition(|c| c.rect.contains(x, y))
    }

    fn slider_value(rect: R, x: f32) -> f64 {
        let t = ((x - rect.x - 6.0) / (rect.w - 12.0)).clamp(0.0, 1.0) as f64;
        ((1.0 + t * 7.0) * 2.0).round() / 2.0
    }

    /// Scrolls so the focused control is on screen.
    fn reveal_focus(&mut self, height: f32) {
        if let Some(c) = self.focus.and_then(|i| self.controls.get(i)) {
            if c.rect.y < 12.0 {
                self.scroll += c.rect.y - 12.0;
            } else if c.rect.y + c.rect.h > height - 12.0 {
                self.scroll += c.rect.y + c.rect.h - height + 12.0;
            }
        }
    }

    pub fn message(&mut self, msg: u32, wp: usize, lp: isize) -> Vec<Action> {
        let mut acts = Vec::new();
        let (cw, ch, k) = self.client();
        let (width, height) = (cw as f32 / k, ch as f32 / k);
        let x = (lp & 0xFFFF) as i16 as f32 / k;
        let y = ((lp >> 16) & 0xFFFF) as i16 as f32 / k;
        match msg {
            WM_SIZE => {}
            WM_CLOSE => return vec![Action::Close],
            WM_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TOOLTIP);
                }
                self.tooltip = true;
            }
            WM_MOUSEMOVE => {
                match self.drag {
                    Some(Drag::Slider(rect)) => acts.push(Action::Duration(Self::slider_value(rect, x))),
                    Some(Drag::Scroll(grab)) => {
                        if let Some(t) = self.thumb(width, height) {
                            self.scroll = (y - grab) / (height - t.h).max(1.0) * (self.content - height);
                        }
                    }
                    None => {}
                }
                let over = self.at(x, y);
                unsafe {
                    SetCursor(LoadCursorW(None, if over.is_some() { IDC_HAND } else { IDC_ARROW }).ok());
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut t);
                }
                if over == self.hover && self.drag.is_none() {
                    return acts; // nothing to redraw
                }
                self.hover = over;
                self.tooltip = false;
                unsafe { SetTimer(Some(self.hwnd), TIMER_TOOLTIP, 600, None) };
            }
            WM_MOUSELEAVE => {
                self.hover = None;
                self.tooltip = false;
            }
            WM_LBUTTONDOWN => {
                self.focus = None;
                self.tooltip = false;
                unsafe { SetCapture(self.hwnd) };
                if let Some(t) = self.thumb(width, height).filter(|t| x >= t.x - 4.0) {
                    let grab = if t.contains(t.x, y) { y - t.y } else { t.h / 2.0 };
                    self.drag = Some(Drag::Scroll(grab));
                } else {
                    self.pressed = self.at(x, y);
                    if let Some(c) = self.pressed.and_then(|i| self.controls.get(i)) {
                        if let Kind::Slider(_) = c.kind {
                            self.drag = Some(Drag::Slider(c.rect));
                            acts.push(Action::Duration(Self::slider_value(c.rect, x)));
                        }
                    }
                }
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                if self.drag.take().is_none() && pressed.is_some() && pressed == self.at(x, y) {
                    if let Some(Control { kind: Kind::Button(a), .. }) = pressed.and_then(|i| self.controls.get(i)) {
                        acts.push(*a);
                    }
                }
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            WM_CAPTURECHANGED => {
                (self.drag, self.pressed) = (None, None);
            }
            WM_MOUSEWHEEL => {
                self.scroll -= (wp >> 16) as i16 as f32 / 120.0 * 64.0;
                self.hover = None;
                self.tooltip = false;
            }
            WM_KEYDOWN => {
                let key = VIRTUAL_KEY(wp as u16);
                let n = self.controls.len();
                let back = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let focused = self.focus.and_then(|i| self.controls.get(i)).map(|c| c.kind);
                match key {
                    VK_ESCAPE => return vec![Action::Close],
                    VK_TAB if n > 0 => {
                        self.focus = Some(match self.focus {
                            None => if back { n - 1 } else { 0 },
                            Some(i) => (i + if back { n - 1 } else { 1 }) % n,
                        });
                        self.reveal_focus(height);
                    }
                    VK_SPACE | VK_RETURN => {
                        if let Some(Kind::Button(a)) = focused {
                            acts.push(a);
                        }
                    }
                    VK_LEFT | VK_RIGHT => {
                        if let Some(Kind::Slider(v)) = focused {
                            acts.push(Action::Duration((v + if key == VK_LEFT { -0.5 } else { 0.5 }).clamp(1.0, 8.0)));
                        }
                    }
                    VK_UP => self.scroll -= 48.0,
                    VK_DOWN => self.scroll += 48.0,
                    VK_PRIOR => self.scroll -= height * 0.8,
                    VK_NEXT => self.scroll += height * 0.8,
                    VK_HOME => self.scroll = 0.0,
                    VK_END => self.scroll = self.content,
                    _ => return acts,
                }
            }
            _ => return acts,
        }
        self.render();
        acts
    }
}

/// The page being laid out, top to bottom.
struct Page<'a> {
    s: &'a Surface,
    x: f32,
    w: f32,
    y: f32,
    controls: &'a mut Vec<Control>,
    hover: Option<usize>,
    pressed: Option<usize>,
}

impl Page<'_> {
    /// Registers a control and says whether the mouse is over it.
    fn control(&mut self, rect: R, kind: Kind, tip: &'static str) -> bool {
        let i = self.controls.len();
        self.controls.push(Control { rect, kind, tip });
        self.hover == Some(i)
    }

    fn section(&mut self, title: &str) {
        self.y += 22.0;
        self.s.text(title, r(self.x + 4.0, self.y, self.w - 8.0, 16.0), bold(12.0), MUTED, Align::Left);
        self.y += 24.0;
    }

    /// A card whose height is only known once its rows are placed: they are laid out twice, first
    /// invisibly to measure.
    fn card(&mut self, rows: &dyn Fn(&mut Page)) {
        let start = self.y;
        let height = {
            // far below the window, where drawing is clipped away
            const AWAY: f32 = 100_000.0;
            let mut scratch = Vec::new();
            let mut ghost = Page { s: self.s, x: self.x, w: self.w, y: AWAY, controls: &mut scratch, hover: None, pressed: None };
            rows(&mut ghost);
            ghost.y - AWAY
        };
        let rect = r(self.x, start, self.w, height);
        self.s.fill(rect, 12.0, CARD);
        self.s.border(rect, 12.0, LINE, 1.0);
        rows(self);
    }

    fn divider(&mut self) {
        self.s.fill(r(self.x + 16.0, self.y, self.w - 32.0, 1.0), 0.0, LINE);
        self.y += 1.0;
    }

    /// A label with an optional hint, leaving `right` units for the control. Returns the row.
    fn row(&mut self, label: &str, hint: &str, right: f32, min: f32) -> R {
        let tw = self.w - 32.0 - right - 12.0;
        let hh = if hint.is_empty() { 0.0 } else { measure(hint, text(12.0), tw, true).1 + 2.0 };
        let block = 19.0 + hh;
        let height = block.max(min) + 24.0;
        let rect = r(self.x, self.y, self.w, height);
        let top = self.y + (height - block) / 2.0;
        self.s.text(label, r(self.x + 16.0, top, tw, 19.0), text(13.5), FG, Align::Left);
        if hh > 0.0 {
            self.s.paragraph(hint, r(self.x + 16.0, top + 21.0, tw, hh), text(12.0), FAINT);
        }
        self.y += height;
        rect
    }

    fn switch(&mut self, rect: R, on: bool, action: Action, tip: &'static str) {
        let over = self.control(rect, Kind::Button(action), tip);
        let track = r(rect.x, rect.y, 40.0, 22.0);
        let o = if over { 0.9 } else { 1.0 };
        self.s.fill(track, 11.0, fade(if on { ACCENT } else { 0x1FFFFFFF }, o));
        if !on {
            self.s.border(track, 11.0, 0x40FFFFFF, 1.0);
        }
        self.s.circle(rect.x + if on { 29.0 } else { 11.0 }, rect.y + 11.0, 7.0, if on { 0xFF111216 } else { 0xFFC8CAD4 });
    }

    fn toggle(&mut self, label: &str, hint: &str, which: Toggle, on: bool) {
        let row = self.row(label, hint, 40.0, 32.0);
        let rect = r(row.x + row.w - 56.0, row.y + (row.h - 22.0) / 2.0, 40.0, 22.0);
        self.switch(rect, on, Action::Set(which, !on), "");
    }

    /// Segmented choice, right-aligned at `right`.
    fn segments(&mut self, right: f32, y: f32, options: &[(&str, bool, Action)]) {
        let widths: Vec<f32> = options.iter().map(|o| measure(o.0, text(12.5), 300.0, false).0 + 24.0).collect();
        let total: f32 = widths.iter().sum::<f32>() + 6.0;
        let mut x = right - total;
        self.s.fill(r(x, y, total, 32.0), 9.0, CARD);
        x += 3.0;
        for ((label, on, action), w) in options.iter().zip(widths) {
            let rect = r(x, y + 3.0, w, 26.0);
            let over = self.control(rect, Kind::Button(*action), "");
            if *on || over {
                self.s.fill(rect, 7.0, if *on { 0x24FFFFFF } else { 0x0FFFFFFF });
            }
            self.s.text(label, rect, text(12.5), if *on || over { FG } else { MUTED }, Align::Center);
            x += w;
        }
    }

    fn pill(&mut self, right: f32, y: f32, label: &str, colour: u32, action: Action, small: bool) -> f32 {
        let (size, pad, h) = if small { (11.5, 10.0, 22.0) } else { (12.5, 14.0, 30.0) };
        let w = measure(label, text(size), 300.0, false).0 + pad * 2.0;
        let rect = r(right - w, y, w, h);
        let i = self.controls.len();
        let over = self.control(rect, Kind::Button(action), "");
        self.s.fill(rect, 8.0, 0x14FFFFFF);
        self.s.border(rect, 8.0, 0x1AFFFFFF, 1.0);
        if over {
            self.s.fill(rect, 8.0, if self.pressed == Some(i) { 0x1FFFFFFF } else { 0x0FFFFFFF });
        }
        self.s.text(label, rect, text(size), colour, Align::Center);
        w
    }

    fn icon_button(&mut self, rect: R, glyph: &str, size: f32, colour: u32, action: Action, tip: &'static str) {
        if self.control(rect, Kind::Button(action), tip) {
            self.s.fill(rect, 15.0, 0x1AFFFFFF);
        }
        self.s.glyph(glyph, rect, size, colour);
    }

    fn contents(&mut self, m: &Model) {
        let st = &m.settings;
        let (x, w) = (self.x, self.w);

        // header
        if let Some(logo) = &m.logo {
            self.s.image(logo, r(x, self.y + 2.0, 44.0, 44.0), 1.0);
        }
        self.s.text("Tempo", r(x + 58.0, self.y, w - 58.0, 31.0), display(24.0), FG, Align::Left);
        self.s.text(&format!("Version {}", crate::updater::VERSION), r(x + 59.0, self.y + 31.0, w - 60.0, 17.0), text(12.0), FAINT, Align::Left);
        self.y += 48.0;

        if m.welcome {
            self.y += 20.0;
            let body = "It runs hidden, with no tray icon. Press a media key and a pop-up shows briefly; it controls the app that's actually playing. Right-click the pop-up for more.\nHold Next / Previous to scrub, and hold Play/Pause to switch apps.\nOpen Tempo from the Start menu to get back here.";
            let bh = measure(body, text(12.0), w - 32.0, true).1;
            let rect = r(x, self.y, w, bh + 54.0);
            self.s.gradient(rect, 12.0, &[(0.0, 0x268AA2FF), (1.0, 0x0A8AA2FF)]);
            self.s.border(rect, 12.0, LINE, 1.0);
            self.s.text("Tempo now handles your media keys", r(x + 16.0, rect.y + 14.0, w - 32.0, 20.0), bold(14.0), FG, Align::Left);
            self.s.paragraph(body, r(x + 16.0, rect.y + 40.0, w - 32.0, bh), text(12.0), MUTED);
            self.y += rect.h;
        }

        // live status
        self.y += 12.0;
        let rect = r(x, self.y, w, 68.0);
        self.s.fill(rect, 12.0, CARD);
        self.s.border(rect, 12.0, LINE, 1.0);
        let art = r(x + 16.0, rect.y + 14.0, 40.0, 40.0);
        self.s.fill(art, 9.0, 0x1FFFFFFF);
        if let Some(a) = &m.status_art {
            self.s.cover(a, art, 9.0, 1.0);
        }
        let bw = self.pill(x + w - 16.0, rect.y + 19.0, "Show pop-up", FG, Action::Preview, false);
        let tw = w - 68.0 - bw - 28.0;
        self.s.text(&m.status_title, r(x + 68.0, rect.y + 15.0, tw, 19.0), bold(13.0), FG, Align::Left);
        self.s.text(&m.status_sub, r(x + 68.0, rect.y + 36.0, tw, 17.0), text(12.0), MUTED, Align::Left);
        self.y += rect.h;

        self.section("Pop-up");
        self.card(&|p| {
            p.toggle("Show the pop-up on key presses", "Off: keys still work, silently.", Toggle::ShowPopup, st.show_popup);
            p.divider();
            // a mini screen with six spots
            let row = p.row("Position", "", 112.0, 68.0);
            let screen = r(row.x + row.w - 128.0, row.y + 12.0, 112.0, 68.0);
            p.s.fill(screen, 8.0, CARD);
            p.s.border(screen, 8.0, 0x26FFFFFF, 1.0);
            for (i, (place, tip)) in [
                (Placement::TopLeft, "Top left"),
                (Placement::TopCenter, "Top center"),
                (Placement::TopRight, "Top right"),
                (Placement::BottomLeft, "Bottom left"),
                (Placement::BottomCenter, "Bottom center"),
                (Placement::BottomRight, "Bottom right"),
            ]
            .into_iter()
            .enumerate()
            {
                let tx = screen.x + 4.0 + (i % 3) as f32 * 37.0;
                let ty = if i < 3 { screen.y + 4.0 } else { screen.y + screen.h - 22.0 };
                let tile = r(tx, ty, 30.0, 18.0);
                let over = p.control(tile, Kind::Button(Action::Place(place)), tip);
                let c = if st.placement == place { ACCENT } else if over { 0x66FFFFFF } else { 0x33FFFFFF };
                p.s.fill(r(tx + 4.0, ty + if i < 3 { 0.0 } else { 11.0 }, 22.0, 7.0), 3.0, c);
            }
            p.divider();
            let row = p.row("Screen", "", 260.0, 32.0);
            p.segments(row.x + row.w - 16.0, row.y + (row.h - 32.0) / 2.0, &[
                ("Where the mouse is", st.follow_mouse, Action::FollowMouse(true)),
                ("Main screen", !st.follow_mouse, Action::FollowMouse(false)),
            ]);
            p.divider();
            let row = p.row("Stays on screen for", "", 200.0, 32.0);
            let slider = r(row.x + row.w - 16.0 - 44.0 - 150.0, row.y + (row.h - 18.0) / 2.0, 150.0, 18.0);
            p.control(slider, Kind::Slider(st.popup_seconds), "");
            let t = ((st.popup_seconds - 1.0) / 7.0).clamp(0.0, 1.0) as f32;
            let bar = r(slider.x, slider.y + 7.0, slider.w, 4.0);
            p.s.fill(bar, 2.0, 0x26FFFFFF);
            p.s.fill(r(bar.x, bar.y, 6.0 + t * (bar.w - 12.0), 4.0), 2.0, ACCENT);
            p.s.circle(bar.x + 6.0 + t * (bar.w - 12.0), bar.y + 2.0, 6.0, FG);
            let value = format!("{} s", (st.popup_seconds * 10.0).round() / 10.0);
            p.s.text(&value, r(row.x + row.w - 60.0, row.y, 44.0, row.h), text(13.0), MUTED, Align::Right);
            p.divider();
            p.toggle("Don't show over fullscreen games and videos", "Keys keep working; only the pop-up is skipped.", Toggle::Fullscreen, st.hide_in_fullscreen);
        });

        self.section("Keys");
        self.card(&|p| {
            p.toggle("Hold Next / Previous to scrub", "Speeds up the longer you hold; release to jump there. A quick tap still skips the track.", Toggle::Scrub, st.hold_to_scrub);
            p.divider();
            p.toggle("Hold Play/Pause to switch app", "Only when more than one app has media. A quick tap still plays or pauses.", Toggle::HoldSwitch, st.hold_to_switch_app);
            p.divider();
            p.toggle("Double-tap Play/Pause to open the app", "Brings the playing app (or the browser window with the video) to the front. Playback stays as it was.", Toggle::DoubleTap, st.double_tap_to_open);
            p.divider();
            p.toggle("Keep media keys from Windows at all times", "Even with nothing open, the keys come to Tempo instead of showing the Windows flyout.", Toggle::AlwaysCapture, st.always_capture);
            p.divider();
            let row = p.row("Skip buttons jump", "", 220.0, 32.0);
            let options: Vec<(String, u32)> = [5, 10, 15, 30].iter().map(|n| (format!("{n} s"), *n)).collect();
            let segments: Vec<(&str, bool, Action)> = options.iter().map(|(l, n)| (l.as_str(), st.skip_seconds == *n, Action::Skip(*n))).collect();
            p.segments(row.x + row.w - 16.0, row.y + (row.h - 32.0) / 2.0, &segments);
        });

        self.section("Apps the keys can control");
        self.card(&|p| {
            if m.apps.is_empty() {
                p.s.text("No media apps seen yet. Play something and it shows up here.", r(p.x + 16.0, p.y, p.w - 32.0, 46.0), text(12.0), FAINT, Align::Left);
                p.y += 46.0;
            }
            for (i, a) in m.apps.iter().enumerate() {
                if i > 0 {
                    p.divider();
                }
                let (x, y, w) = (p.x, p.y, p.w);
                p.s.fill(r(x + 16.0, y + 14.0, 28.0, 28.0), 7.0, LINE);
                if let Some(icon) = &a.icon {
                    p.s.image(icon, r(x + 20.0, y + 18.0, 20.0, 20.0), 1.0);
                }
                p.s.text(&a.name, r(x + 56.0, y + 9.0, w - 56.0 - 118.0, 19.0), text(13.5), FG, Align::Left);
                p.s.text(&a.sub, r(x + 56.0, y + 30.0, w - 56.0 - 118.0, 17.0), text(12.0), FAINT, Align::Left);
                let (glyph, colour, tip) = if a.preferred {
                    ("\u{E735}", ACCENT, "Preferred: gets the keys when nothing is playing")
                } else {
                    ("\u{E734}", FAINT, "Prefer this app when nothing is playing")
                };
                p.icon_button(r(x + w - 106.0, y + 13.0, 30.0, 30.0), glyph, 14.0, colour, Action::Prefer(i), tip);
                p.switch(r(x + w - 56.0, y + 17.0, 40.0, 22.0), a.enabled, Action::AppEnabled(i, !a.enabled), "Let the media keys control this app");
                p.y += 56.0;
            }
            p.divider();
            p.toggle("Let other players have the keys while they play", "For players that don't report to Windows (VLC 3, MPC-HC, PotPlayer, Winamp…): while one is playing, keys go to it instead.", Toggle::PassThrough, st.pass_through_unknown_players);
        });

        self.section("Recently played");
        if !m.history.is_empty() {
            self.pill(x + w, self.y - 28.0, "Clear", FG, Action::ClearHistory, true);
        }
        self.card(&|p| {
            if m.history.is_empty() {
                p.s.text("Tracks you play show up here, so you can find that song again.", r(p.x + 16.0, p.y, p.w - 32.0, 46.0), text(12.0), FAINT, Align::Left);
                p.y += 46.0;
            }
            for (i, h) in m.history.iter().enumerate() {
                if i > 0 {
                    p.divider();
                }
                let (x, y, w) = (p.x, p.y, p.w);
                if let Some(icon) = &h.icon {
                    p.s.image(icon, r(x + 16.0, y + 17.0, 18.0, 18.0), 1.0);
                }
                p.s.text(&h.title, r(x + 46.0, y + 7.0, w - 46.0 - 108.0, 19.0), text(13.0), FG, Align::Left);
                p.s.text(&h.sub, r(x + 46.0, y + 27.0, w - 46.0 - 108.0, 17.0), text(11.5), FAINT, Align::Left);
                for (n, (glyph, action, tip)) in [("\u{E721}", Action::YouTube(i), "Search on YouTube"), ("\u{E8D6}", Action::Spotify(i), "Search on Spotify"), ("\u{E8C8}", Action::Copy(i), "Copy song name")].into_iter().enumerate() {
                    p.icon_button(r(x + w - 100.0 + n as f32 * 30.0, y + 11.0, 30.0, 30.0), glyph, 13.0, MUTED, action, tip);
                }
                p.y += 52.0;
            }
        });

        self.section("Stats");
        self.card(&|p| {
            let cell = (p.w - 16.0) / 3.0;
            let mut y = p.y + 12.0;
            for row in m.stats.chunks(3) {
                let label_h = row.iter().map(|t| measure(t.1, text(11.5), cell - 16.0, true).1).fold(0.0, f32::max);
                for (i, (value, label)) in row.iter().enumerate() {
                    let x = p.x + 16.0 + i as f32 * cell;
                    p.s.text(value, r(x, y + 6.0, cell - 16.0, 27.0), display(20.0), FG, Align::Left);
                    p.s.paragraph(label, r(x, y + 33.0, cell - 16.0, label_h), text(11.5), FAINT);
                }
                y += label_h + 45.0;
            }
            p.y = y + 12.0;
        });
        self.y += 6.0;
        let since_h = measure(&m.stats_since, text(12.0), w - 12.0, true).1;
        self.s.paragraph(&m.stats_since, r(x + 6.0, self.y, w - 12.0, since_h), text(12.0), FAINT);
        self.y += since_h;

        self.section("General");
        self.card(&|p| {
            let row = p.row("Update automatically", &m.update_status, 160.0, 32.0);
            let y = row.y + (row.h - 30.0) / 2.0;
            p.pill(row.x + row.w - 68.0, y, "Check now", FG, Action::CheckUpdates, false);
            p.switch(r(row.x + row.w - 56.0, y + 4.0, 40.0, 22.0), st.auto_update, Action::Set(Toggle::AutoUpdate, !st.auto_update), "Update automatically");
            p.divider();
            p.toggle("Start with Windows", "", Toggle::Startup, m.starts_with_windows);
            p.divider();
            let row = p.row("Quit Tempo", "Media keys go back to Windows until Tempo is opened again.", 70.0, 32.0);
            p.pill(row.x + row.w - 16.0, row.y + (row.h - 30.0) / 2.0, "Quit", 0xFFFF8A8A, Action::Quit, false);
        });
    }
}
