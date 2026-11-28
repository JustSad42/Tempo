//! Tempo: better media keys for Windows. Runs hidden; media keys control the app that's actually
//! playing, with a small pop-up on each press.
//!
//! One thread owns all state. Everything else (the keyboard hook, Windows' media callbacks, timers,
//! window messages) only `post`s an `Event`; the message loop handles them one at a time.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod apps;
mod art;
mod config;
mod gfx;
mod hook;
mod logic;
mod media;
mod popup;
mod settings_ui;
mod snapshot;
mod supervisor;
mod system;
mod updater;

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering::SeqCst};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub enum Signal {
    ShowSettings,
    Preview,
    /// Test copies only.
    DropHook,
}

pub enum Event {
    Key(logic::Key, bool),
    Manager(windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager),
    Sessions,
    CurrentSession,
    Props(u64),
    PropsRead(media::Props),
    Playback(u64),
    Timeline(u64),
    /// A timer of the main window.
    Timer(usize),
    /// A message of the pop-up window.
    Popup(u32, usize, isize),
    /// A message of the settings window.
    Settings(u32, usize, isize),
    /// `WM_INPUT` saw a media key go down.
    RawMediaKey,
    Resumed,
    Signal(Signal),
    UpdateStatus,
    UpdateReady,
}

static QUEUE: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
/// The hidden main window, woken when something is queued.
pub static HOST: AtomicIsize = AtomicIsize::new(0);

pub fn post(event: Event) {
    QUEUE.lock().unwrap().push_back(event);
    let host = HOST.load(SeqCst);
    if host != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(host as _)), WM_APP, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn next_event() -> Option<Event> {
    QUEUE.lock().unwrap().pop_front()
}

/// Handles queued events now, unless that is already happening further up the stack.
pub fn pump() {
    app::pump();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    std::panic::set_hook(Box::new(|info| config::log(&format!("Fatal: {info}"))));

    if has("--dump") {
        let file = args.iter().skip_while(|a| *a != "--dump").nth(1).map_or_else(|| config::dir().join("sessions.json"), Into::into);
        snapshot::dump(&file);
        return;
    }
    // Pictures of the pop-up and settings with made-up tracks, for checking how they look.
    if let Some(dir) = args.iter().skip_while(|a| *a != "--snapshot").nth(1) {
        snapshot::render(std::path::Path::new(dir));
        return;
    }
    if has("--test-drop-hook") && !config::instance_tag().is_empty() {
        supervisor::signal("TestDropHook");
        return;
    }
    if !has("--child") {
        supervisor::run(&args);
        return;
    }
    app::run(&args);
}
