//! Developer tools: `--snapshot <dir>` draws the pop-up and settings with made-up tracks to PNG
//! files; `--dump <file>` writes the current media sessions as JSON.

use crate::art::{self, Image};
use crate::config::Settings;
use crate::gfx::Surface;
use crate::logic::Timeline;
use crate::media::Hub;
use crate::popup::{self, Popup, Source, Track, View};
use crate::settings_ui as ui;
use crate::{Event, apps};
use std::path::Path;
use std::rc::Rc;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

pub fn dump(file: &Path) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let settings: Settings = crate::config::load("settings.json").unwrap_or_default();
    let mut hub = Hub::default();
    let manager = windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync().and_then(|op| op.join());
    let mut error = None;
    match manager {
        Ok(m) => hub.connect(m),
        Err(e) => error = Some(e.to_string()),
    }
    std::thread::sleep(std::time::Duration::from_millis(1500));
    while let Some(e) = crate::next_event() {
        if let Event::PropsRead(p) = e {
            hub.apply_props(p);
        }
    }
    let target = hub.target(&settings);
    let data = serde_json::json!({
        "error": error,
        "target": target.and_then(|k| hub.get(k)).map(|s| s.app.aumid.clone()),
        "sessions": hub.sessions.iter().map(|s| serde_json::json!({
            "aumid": s.app.aumid, "app": s.app.name, "exes": s.app.exes, "hasIcon": s.app.icon.is_some(),
            "title": s.title, "artist": s.artist, "playing": s.playing, "hasArt": s.art.is_some(),
            "accent": format!("{:08X}", s.accent), "position": crate::logic::format_time(s.position()),
            "duration": crate::logic::format_time(s.timeline.duration),
            "canNext": s.can_next, "canPrevious": s.can_previous, "canSeek": s.can_seek,
            "volume": apps::volume(&s.app.audio_id()).map(|v| v.0),
        })).collect::<Vec<_>>(),
        "audio": apps::audio_sessions().iter().map(|a| serde_json::json!({"pid": a.pid, "exe": a.exe, "aumid": a.aumid})).collect::<Vec<_>>(),
    });
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(file, serde_json::to_string_pretty(&data).unwrap_or_default());
}

fn sample_art() -> Rc<Image> {
    let mut i = Image::new(320, 320);
    for (n, p) in i.px.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((n % 320) as u32, (n / 320) as u32);
        let orange = x + 320 - y < 300;
        p.copy_from_slice(&if orange { [60, 136, 255, 255] } else { [112, 32, 64, 255] });
    }
    Rc::new(i)
}

fn sample_view() -> View {
    let art = sample_art();
    let source = |key, name: &str, playing| Source { key, name: name.into(), title: "Something".into(), icon: None, playing };
    View {
        target: Some(Track {
            key: 1,
            app: "Spotify".into(),
            icon: None,
            title: "Golden Hour Drive".into(),
            artist: "The Velvet Tides".into(),
            accent: art::accent(&art),
            art: Some(art),
            playing: true,
            can_previous: true,
            can_next: true,
            can_seek: true,
            timeline: Timeline { position: 86.0, duration: 214.0, at_ms: 0, rate: 1.0 },
        }),
        sources: vec![source(1, "Spotify", true), source(2, "Chrome", false), source(3, "Edge", false)],
        volume: Some((0.7, false)),
        skip: 10,
        preferred: false,
    }
}

fn sample_model() -> ui::Model {
    let row = |name: &str, sub: &str, preferred| ui::AppRow { id: name.into(), name: name.into(), icon: None, sub: sub.into(), preferred, enabled: true };
    let track = |title: &str, sub: &str| ui::TrackRow { icon: None, title: title.into(), sub: sub.into() };
    let logo = Rc::new(Image { w: 64, h: 64, px: include_bytes!("../assets/logo-64.bgra").to_vec() });
    ui::Model {
        settings: Settings::default(),
        starts_with_windows: true,
        welcome: true,
        status_art: Some(sample_art()),
        status_title: "Golden Hour Drive".into(),
        status_sub: "Keys control Spotify · Playing · 3 apps with media".into(),
        apps: vec![row("Google Chrome", "Open, paused · Lo-fi beats to study to - 24/7 mix", false), row("Spotify", "Preferred · Playing now · Golden Hour Drive", true)],
        history: vec![track("Golden Hour Drive", "The Velvet Tides · 4 min ago"), track("أغنية تجريبية", "فنان · 27 min ago")],
        stats: vec![("12".into(), "key presses today"), ("3,481".into(), "key presses in total"), ("204".into(), "went to a different app than Windows would have"), ("31".into(), "scrubs"), ("18".into(), "app switches"), ("0".into(), "times the key hook was reconnected")],
        stats_since: "Counting since 3 Nov 2026. 12 key presses were handed to players that don't report to Windows.".into(),
        update_status: "Checks GitHub every few hours and installs quietly.".into(),
        logo: Some(logo),
    }
}

pub fn render(dir: &Path) {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    let _ = std::fs::create_dir_all(dir);
    let scale = 1.5;
    let (w, h) = popup::size(scale);
    let empty = View { skip: 10, ..Default::default() };
    for (name, view, menu, scrub) in [
        ("popup", sample_view(), false, None),
        ("menu", sample_view(), true, None),
        ("scrub", sample_view(), false, Some((131.0, 86.0))),
        ("empty", empty, false, None),
    ] {
        let mut p = Popup::offscreen();
        p.pose(view, menu, scrub);
        let Some(s) = Surface::new(w, h) else { return };
        p.draw(&s, scale, crate::system::now_ms());
        save(&dir.join(format!("{name}.png")), &s);
    }
    // the whole settings page, in one tall picture
    let mut window = ui::Window::offscreen(sample_model());
    let (w, h) = (560.0, 1900.0);
    if let Some(s) = Surface::new((w * scale) as i32, (h * scale) as i32) {
        window.draw(&s, w, h, scale);
        save(&dir.join("settings.png"), &s);
    }
}

/// Writes the surface over a grey backdrop as an uncompressed PNG.
fn save(path: &Path, s: &Surface) {
    let mut raw = Vec::with_capacity(((s.w * 3 + 1) * s.h) as usize);
    for row in s.pixels().chunks_exact(s.w as usize * 4) {
        raw.push(0);
        for p in row.chunks_exact(4) {
            let under = (255 - p[3] as u32) * 0x60 / 255; // premultiplied "over" grey
            raw.extend([p[2], p[1], p[0]].map(|c| (c as u32 + under).min(255) as u8));
        }
    }
    let _ = std::fs::write(path, png(s.w as u32, s.h as u32, &raw));
}

fn crc(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, b| (0..8).fold(c ^ *b as u32, |c, _| if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }))
}

/// A PNG of 8-bit RGB rows, each led by a zero filter byte, stored without compression.
fn png(w: u32, h: u32, rows: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let blocks = rows.chunks(65535).count();
    for (i, b) in rows.chunks(65535).enumerate() {
        z.push((i + 1 == blocks) as u8);
        z.extend((b.len() as u16).to_le_bytes());
        z.extend((!(b.len() as u16)).to_le_bytes());
        z.extend(b);
    }
    let (a, b) = rows.iter().fold((1u32, 0u32), |(a, b), x| {
        let a = (a + *x as u32) % 65521;
        (a, (b + a) % 65521)
    });
    z.extend((b << 16 | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend((data.len() as u32).to_be_bytes());
        let body = [kind, data].concat();
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    };
    let mut head = [w.to_be_bytes(), h.to_be_bytes()].concat();
    head.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &head);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}
