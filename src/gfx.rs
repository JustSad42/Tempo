//! Drawing: Direct2D (software) and DirectWrite into a memory bitmap, which the pop-up hands to
//! Windows as a per-pixel transparent window and the settings window copies to its client area.
//! No graphics device is created, so no GPU driver is loaded into the process.
//!
//! All coordinates are design units (96 dpi); `begin` sets the scale.

use crate::art::Image;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows_core::{Interface, PCWSTR};
use windows_numerics::{Matrix3x2, Vector2};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub const fn r(x: f32, y: f32, w: f32, h: f32) -> R {
    R { x, y, w, h }
}

impl R {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn inset(&self, d: f32) -> R {
        r(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

/// `colour` (0xAARRGGBB) with its alpha multiplied by `opacity`.
pub fn fade(colour: u32, opacity: f32) -> u32 {
    let a = ((colour >> 24) as f32 * opacity.clamp(0.0, 1.0)).round() as u32;
    (colour & 0x00FF_FFFF) | (a << 24)
}

pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let part = |shift: u32| {
        let (x, y) = ((a >> shift & 255) as f32, (b >> shift & 255) as f32);
        ((x + (y - x) * t.clamp(0.0, 1.0)).round() as u32) << shift
    };
    part(24) | part(16) | part(8) | part(0)
}

fn color(c: u32) -> D2D1_COLOR_F {
    let f = |s: u32| (c >> s & 255) as f32 / 255.0;
    D2D1_COLOR_F { r: f(16), g: f(8), b: f(0), a: f(24) }
}

// ---------- vector icons ----------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Cubic([f32; 6]),
    Arc { x: f32, y: f32, rx: f32, ry: f32, rotation: f32, large: bool, clockwise: bool },
    Close,
}

/// The subset of SVG path data the icons use (M L H V C A Z, absolute and relative).
pub fn parse_path(data: &str) -> Vec<Seg> {
    let b = data.as_bytes();
    let mut i = 0;
    let number = |i: &mut usize| -> Option<f32> {
        while *i < b.len() && (b[*i] == b' ' || b[*i] == b',') {
            *i += 1;
        }
        let start = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        let mut dot = false;
        while *i < b.len() && (b[*i].is_ascii_digit() || (b[*i] == b'.' && !dot)) {
            dot |= b[*i] == b'.';
            *i += 1;
        }
        data[start..*i].parse().ok()
    };
    let mut out = Vec::new();
    let (mut x, mut y, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut cmd = b' ';
    while i < b.len() {
        if b[i] == b' ' || b[i] == b',' {
            i += 1;
            continue;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
            if cmd.eq_ignore_ascii_case(&b'z') {
                out.push(Seg::Close);
                (x, y) = (sx, sy);
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        let step = (|| -> Option<()> {
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    (x, y) = (ox + number(&mut i)?, oy + number(&mut i)?);
                    (sx, sy) = (x, y);
                    out.push(Seg::Move(x, y));
                    cmd = if rel { b'l' } else { b'L' }; // further pairs are lines
                }
                b'L' => {
                    (x, y) = (ox + number(&mut i)?, oy + number(&mut i)?);
                    out.push(Seg::Line(x, y));
                }
                b'H' => {
                    x = ox + number(&mut i)?;
                    out.push(Seg::Line(x, y));
                }
                b'V' => {
                    y = oy + number(&mut i)?;
                    out.push(Seg::Line(x, y));
                }
                b'C' => {
                    let c = [ox + number(&mut i)?, oy + number(&mut i)?, ox + number(&mut i)?, oy + number(&mut i)?, ox + number(&mut i)?, oy + number(&mut i)?];
                    (x, y) = (c[4], c[5]);
                    out.push(Seg::Cubic(c));
                }
                b'A' => {
                    let (rx, ry, rotation) = (number(&mut i)?, number(&mut i)?, number(&mut i)?);
                    let (large, clockwise) = (number(&mut i)? != 0.0, number(&mut i)? != 0.0);
                    (x, y) = (ox + number(&mut i)?, oy + number(&mut i)?);
                    out.push(Seg::Arc { x, y, rx, ry, rotation, large, clockwise });
                }
                _ => return None,
            }
            Some(())
        })();
        if step.is_none() {
            break;
        }
    }
    out
}

// ---------- fonts ----------

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Text,
    Display,
    Icon,
}

#[derive(Clone, Copy)]
pub struct Font {
    pub face: Face,
    pub size: f32,
    pub weight: u32,
}

pub const fn text(size: f32) -> Font {
    Font { face: Face::Text, size, weight: 400 }
}
pub const fn bold(size: f32) -> Font {
    Font { face: Face::Text, size, weight: 600 }
}
pub const fn display(size: f32) -> Font {
    Font { face: Face::Display, size, weight: 600 }
}
pub const fn icon(size: f32) -> Font {
    Font { face: Face::Icon, size, weight: 400 }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

struct Factories {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    /// Family names that exist here, in `Face` order.
    families: [Vec<u16>; 3],
    formats: RefCell<HashMap<(Face, u32, u32), IDWriteTextFormat>>,
    paths: RefCell<HashMap<&'static str, ID2D1PathGeometry>>,
    round_cap: Option<ID2D1StrokeStyle>,
}

impl Factories {
    fn new() -> windows_core::Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut fonts = None;
            dw.GetSystemFontCollection(&mut fonts, false)?;
            let pick = |names: &[&str]| -> Vec<u16> {
                let wide = |n: &str| n.encode_utf16().chain([0]).collect::<Vec<u16>>();
                let exists = |n: &str| {
                    let (mut index, mut found) = (0, Default::default());
                    fonts.as_ref().is_some_and(|f| f.FindFamilyName(PCWSTR(wide(n).as_ptr()), &mut index, &mut found).is_ok() && found.into())
                };
                wide(names.iter().find(|n| exists(n)).unwrap_or(&names[names.len() - 1]))
            };
            let families = [
                pick(&["Segoe UI Variable Text", "Segoe UI"]),
                pick(&["Segoe UI Variable Display", "Segoe UI"]),
                pick(&["Segoe Fluent Icons", "Segoe MDL2 Assets"]),
            ];
            let cap = D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() };
            let round_cap = d2d.CreateStrokeStyle(&cap, None).ok();
            Ok(Factories { d2d, dw, families, formats: Default::default(), paths: Default::default(), round_cap })
        }
    }

    fn format(&self, f: Font) -> Option<IDWriteTextFormat> {
        let key = (f.face, (f.size * 10.0) as u32, f.weight);
        if let Some(x) = self.formats.borrow().get(&key) {
            return Some(x.clone());
        }
        let made = unsafe {
            let family = PCWSTR(self.families[f.face as usize].as_ptr());
            let locale: Vec<u16> = "en-us\0".encode_utf16().collect();
            self.dw
                .CreateTextFormat(family, None, DWRITE_FONT_WEIGHT(f.weight as i32), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, f.size, PCWSTR(locale.as_ptr()))
                .ok()?
        };
        self.formats.borrow_mut().insert(key, made.clone());
        Some(made)
    }

    /// A text block: one line trimmed with an ellipsis, or wrapped to `w`.
    fn layout(&self, s: &str, f: Font, w: f32, h: f32, align: Align, wrap: bool) -> Option<IDWriteTextLayout> {
        let format = self.format(f)?;
        let s: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let l = self.dw.CreateTextLayout(&s, &format, w.max(0.0), h.max(0.0)).ok()?;
            let _ = l.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            if wrap {
                let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            } else {
                let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let _ = l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                if let Ok(ellipsis) = self.dw.CreateEllipsisTrimmingSign(&format) {
                    let _ = l.SetTrimming(&trim, &ellipsis);
                }
            }
            Some(l)
        }
    }

    fn path(&self, data: &'static str) -> Option<ID2D1PathGeometry> {
        if let Some(g) = self.paths.borrow().get(data) {
            return Some(g.clone());
        }
        let made = unsafe {
            let g = self.d2d.CreatePathGeometry().ok()?;
            let sink = g.Open().ok()?;
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            let mut open = false;
            let at = |x: f32, y: f32| Vector2 { X: x, Y: y };
            for seg in parse_path(data) {
                match seg {
                    Seg::Move(x, y) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        sink.BeginFigure(at(x, y), D2D1_FIGURE_BEGIN_FILLED);
                        open = true;
                    }
                    Seg::Line(x, y) if open => sink.AddLine(at(x, y)),
                    Seg::Cubic(c) if open => sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: at(c[0], c[1]), point2: at(c[2], c[3]), point3: at(c[4], c[5]) }),
                    Seg::Arc { x, y, rx, ry, rotation, large, clockwise } if open => sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: at(x, y),
                        size: D2D_SIZE_F { width: rx, height: ry },
                        rotationAngle: rotation,
                        sweepDirection: if clockwise { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    }),
                    Seg::Close if open => {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                    _ => {}
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close().ok()?;
            g
        };
        self.paths.borrow_mut().insert(data, made.clone());
        Some(made)
    }
}

thread_local!(static FACT: &'static Factories = Box::leak(Box::new(Factories::new().expect("Direct2D"))));

fn fact() -> &'static Factories {
    FACT.with(|f| *f)
}

/// Size of a text block in design units.
pub fn measure(s: &str, f: Font, max_width: f32, wrap: bool) -> (f32, f32) {
    let Some(l) = fact().layout(s, f, max_width, 10_000.0, Align::Left, wrap) else { return (0.0, f.size * 1.4) };
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        // Paragraph alignment doesn't change the measured size.
        let _ = l.GetMetrics(&mut m);
    }
    (m.widthIncludingTrailingWhitespace.min(max_width), m.height)
}

// ---------- surface ----------

/// A top-down 32-bit premultiplied bitmap with a Direct2D target on it.
pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *const u8,
    pub w: i32,
    pub h: i32,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    scale: std::cell::Cell<f32>,
    /// A zoom applied on top of every `transform` (the pop-up growing as it appears).
    base: std::cell::Cell<(f32, f32, f32)>,
    /// Uploaded images, keyed by the image they came from (kept alive so the key stays unique).
    images: RefCell<HashMap<usize, (Rc<Image>, ID2D1Bitmap)>>,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Surface> {
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let old = SelectObject(dc, bitmap.into());
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let rt = fact().d2d.CreateDCRenderTarget(&props).ok()?;
            rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h }).ok()?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color(0xFFFF_FFFF), None).ok()?;
            Some(Surface { dc, bitmap, old, bits: bits as *const u8, w, h, rt, brush, scale: 1.0.into(), base: (1.0, 0.0, 0.0).into(), images: Default::default() })
        }
    }

    /// The pixels as drawn (valid after `end`).
    pub fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize) }
    }

    pub fn begin(&self, scale: f32, clear: u32) {
        self.scale.set(scale);
        self.base.set((1.0, 0.0, 0.0));
        unsafe {
            self.rt.BeginDraw();
            self.rt.SetTransform(&Matrix3x2::identity());
            self.rt.Clear(Some(&color(clear)));
            self.transform(1.0, 0.0, 0.0);
        }
    }

    pub fn end(&self) {
        unsafe {
            let _ = self.rt.EndDraw(None, None);
            let _ = GdiFlush();
        }
    }

    /// Zooms everything drawn after this by `z` around the design point (cx, cy).
    pub fn zoom(&self, z: f32, cx: f32, cy: f32) {
        self.base.set((z, cx, cy));
        self.transform(1.0, 0.0, 0.0);
    }

    /// Everything after this is also scaled by `k` around (cx, cy); `1.0` ends it.
    pub fn transform(&self, k: f32, cx: f32, cy: f32) {
        let s = self.scale.get();
        let (z, bx, by) = self.base.get();
        let shift = |c: f32, b: f32| ((c - c * k - b) * z + b) * s;
        let m = Matrix3x2 { M11: k * z * s, M12: 0.0, M21: 0.0, M22: k * z * s, M31: shift(cx, bx), M32: shift(cy, by) };
        unsafe { self.rt.SetTransform(&m) }
    }

    fn solid(&self, c: u32) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(c)) };
        &self.brush
    }

    fn rounded(rect: R, radius: f32) -> D2D1_ROUNDED_RECT {
        let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
        D2D1_ROUNDED_RECT { rect: rect.d2d(), radiusX: radius, radiusY: radius }
    }

    pub fn fill(&self, rect: R, radius: f32, c: u32) {
        if rect.w > 0.0 && rect.h > 0.0 && c >> 24 != 0 {
            unsafe { self.rt.FillRoundedRectangle(&Self::rounded(rect, radius), self.solid(c)) }
        }
    }

    /// A 1-unit-wide (or `width`) border just inside `rect`.
    pub fn border(&self, rect: R, radius: f32, c: u32, width: f32) {
        if c >> 24 != 0 {
            let rr = Self::rounded(rect.inset(width / 2.0), (radius - width / 2.0).max(0.0));
            unsafe { self.rt.DrawRoundedRectangle(&rr, self.solid(c), width, None) }
        }
    }

    fn ellipse_of(cx: f32, cy: f32, radius: f32) -> D2D1_ELLIPSE {
        D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: radius, radiusY: radius }
    }

    pub fn circle(&self, cx: f32, cy: f32, radius: f32, c: u32) {
        if c >> 24 != 0 {
            unsafe { self.rt.FillEllipse(&Self::ellipse_of(cx, cy, radius), self.solid(c)) }
        }
    }

    pub fn ring(&self, cx: f32, cy: f32, radius: f32, c: u32, width: f32) {
        if c >> 24 != 0 {
            unsafe { self.rt.DrawEllipse(&Self::ellipse_of(cx, cy, radius), self.solid(c), width, None) }
        }
    }

    fn stops(&self, stops: &[(f32, u32)]) -> Option<ID2D1GradientStopCollection> {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: color(*c) }).collect();
        unsafe { self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok() }
    }

    /// A diagonal gradient from the top left to the bottom right corner.
    pub fn gradient(&self, rect: R, radius: f32, stops: &[(f32, u32)]) {
        let Some(s) = self.stops(stops) else { return };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: rect.x, Y: rect.y }, endPoint: Vector2 { X: rect.x + rect.w, Y: rect.y + rect.h } };
        unsafe {
            if let Ok(b) = self.rt.CreateLinearGradientBrush(&props, None, &s) {
                self.rt.FillRoundedRectangle(&Self::rounded(rect, radius), &b);
            }
        }
    }

    /// A soft round glow filling `rect`.
    pub fn glow(&self, rect: R, c: u32) {
        let Some(s) = self.stops(&[(0.0, c), (1.0, c & 0x00FF_FFFF)]) else { return };
        let (cx, cy) = rect.center();
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: Vector2 { X: cx, Y: cy }, gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 }, radiusX: rect.w / 2.0, radiusY: rect.h / 2.0 };
        unsafe {
            if let Ok(b) = self.rt.CreateRadialGradientBrush(&props, None, &s) {
                let e = D2D1_ELLIPSE { point: props.center, radiusX: props.radiusX, radiusY: props.radiusY };
                self.rt.FillEllipse(&e, &b);
            }
        }
    }

    fn upload(&self, img: &Rc<Image>) -> Option<ID2D1Bitmap> {
        let key = Rc::as_ptr(img) as usize;
        if let Some((_, b)) = self.images.borrow().get(&key) {
            return Some(b.clone());
        }
        if img.w == 0 || img.h == 0 {
            return None;
        }
        let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
        let made = unsafe { self.rt.CreateBitmap(D2D_SIZE_U { width: img.w, height: img.h }, Some(img.px.as_ptr() as _), img.w * 4, &props).ok()? };
        let mut cache = self.images.borrow_mut();
        // Old tracks' art isn't needed again: keep the cache small.
        if cache.len() >= 96 {
            cache.clear();
        }
        cache.insert(key, (img.clone(), made.clone()));
        Some(made)
    }

    /// Draws the whole image stretched into `rect`.
    pub fn image(&self, img: &Rc<Image>, rect: R, opacity: f32) {
        if let Some(b) = self.upload(img) {
            unsafe { self.rt.DrawBitmap(&b, Some(&rect.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) }
        }
    }

    /// Fills a rounded rectangle with the image, scaled to cover it and centred.
    pub fn cover(&self, img: &Rc<Image>, rect: R, radius: f32, opacity: f32) {
        let Some(b) = self.upload(img) else { return };
        let k = (rect.w / img.w as f32).max(rect.h / img.h as f32);
        let (dx, dy) = (rect.x + (rect.w - img.w as f32 * k) / 2.0, rect.y + (rect.h - img.h as f32 * k) / 2.0);
        let bitmap = D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR };
        let brush = D2D1_BRUSH_PROPERTIES { opacity, transform: Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: dx, M32: dy } };
        unsafe {
            if let Ok(bb) = self.rt.CreateBitmapBrush(&b, Some(&bitmap), Some(&brush)) {
                self.rt.FillRoundedRectangle(&Self::rounded(rect, radius), &bb);
            }
        }
    }

    /// Clips everything until `pop_clip` to a rounded rectangle.
    pub fn push_clip(&self, rect: R, radius: f32) {
        unsafe {
            let geometry = fact().d2d.CreateRoundedRectangleGeometry(&Self::rounded(rect, radius)).ok().and_then(|g| g.cast::<ID2D1Geometry>().ok());
            let mut p = D2D1_LAYER_PARAMETERS {
                contentBounds: rect.d2d(),
                geometricMask: std::mem::ManuallyDrop::new(geometry),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            let layer = self.rt.CreateLayer(None).ok();
            self.rt.PushLayer(&p, layer.as_ref());
            std::mem::ManuallyDrop::drop(&mut p.geometricMask);
        }
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt.PopLayer() }
    }

    /// One line, vertically centred, trimmed with an ellipsis.
    pub fn text(&self, s: &str, rect: R, f: Font, c: u32, align: Align) {
        self.draw_text(s, rect, f, c, align, false)
    }

    /// Wrapped from the top of `rect`.
    pub fn paragraph(&self, s: &str, rect: R, f: Font, c: u32) {
        self.draw_text(s, rect, f, c, Align::Left, true)
    }

    fn draw_text(&self, s: &str, rect: R, f: Font, c: u32, align: Align, wrap: bool) {
        if c >> 24 == 0 {
            return;
        }
        if let Some(l) = fact().layout(s, f, rect.w, rect.h, align, wrap) {
            unsafe { self.rt.DrawTextLayout(Vector2 { X: rect.x, Y: rect.y }, &l, self.solid(c), D2D1_DRAW_TEXT_OPTIONS_CLIP) }
        }
    }

    /// An icon-font glyph centred in `rect`.
    pub fn glyph(&self, g: &str, rect: R, size: f32, c: u32) {
        self.draw_text(g, rect, icon(size), c, Align::Center, false)
    }

    /// A 24×24 vector icon scaled into `rect`; `mirror` flips it left to right; `stroke` outlines
    /// it with round caps instead of filling.
    pub fn path(&self, data: &'static str, rect: R, c: u32, mirror: bool, stroke: Option<f32>) {
        let Some(g) = fact().path(data) else { return };
        if c >> 24 == 0 {
            return;
        }
        let mut old = Matrix3x2::identity();
        let k = rect.w / 24.0;
        let (sx, tx) = if mirror { (-k, rect.x + rect.w) } else { (k, rect.x) };
        unsafe {
            self.rt.GetTransform(&mut old);
            let m = Matrix3x2 { M11: sx * old.M11, M12: 0.0, M21: 0.0, M22: k * old.M22, M31: tx * old.M11 + old.M31, M32: rect.y * old.M22 + old.M32 };
            self.rt.SetTransform(&m);
            match stroke {
                Some(width) => self.rt.DrawGeometry(&g, self.solid(c), width, fact().round_cap.as_ref()),
                None => self.rt.FillGeometry(&g, self.solid(c), None),
            }
            self.rt.SetTransform(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_data() {
        let p = parse_path("M8 5.14v13.72c0 .79.87 1.27 1.54.84l10.4-6.86a1 1 0 0 0 0-1.68z");
        assert_eq!(p[0], Seg::Move(8.0, 5.14));
        assert_eq!(p[1], Seg::Line(8.0, 5.14 + 13.72));
        assert!(matches!(p[2], Seg::Cubic(c) if (c[2] - 8.87).abs() < 1e-4 && (c[3] - 20.13).abs() < 1e-4));
        assert!(matches!(p[4], Seg::Arc { rx, large: false, clockwise: false, .. } if rx == 1.0));
        assert_eq!(*p.last().unwrap(), Seg::Close);
        // pairs after M are lines; Z returns to the start of the figure
        assert_eq!(parse_path("M1 1 2 2zl1 0")[1], Seg::Line(2.0, 2.0));
        assert_eq!(parse_path("M1 1 2 2zl1 0")[3], Seg::Line(2.0, 1.0));
    }

    #[test]
    fn colours() {
        assert_eq!(fade(0xFF10_2030, 0.5), 0x8010_2030);
        assert_eq!(mix(0xFF00_0000, 0xFFFF_FFFF, 0.5), 0xFF80_8080);
    }

    #[test]
    fn draws_without_a_window() {
        let s = Surface::new(40, 20).unwrap();
        s.begin(1.0, 0);
        s.fill(r(0.0, 0.0, 20.0, 20.0), 0.0, 0xFFFF_0000);
        s.end();
        assert_eq!(&s.pixels()[..4], &[0, 0, 255, 255]);
        assert_eq!(s.pixels()[30 * 4 + 3], 0, "untouched pixels stay transparent");
        assert!(measure("Hello", text(12.0), 500.0, false).0 > 10.0);
    }
}
