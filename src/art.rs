//! Album art: an accent colour from it, black bars trimmed off video thumbnails, a tiny copy that
//! becomes the blurred backdrop, and the soft shadow behind the card. All plain pixel work.

/// Premultiplied BGRA pixels, top row first.
#[derive(Clone, Default)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Image {
    pub fn new(w: u32, h: u32) -> Self {
        Image { w, h, px: vec![0; (w * h * 4) as usize] }
    }

    fn rgb(&self, x: u32, y: u32) -> (u32, u32, u32) {
        let i = ((y * self.w + x) * 4) as usize;
        (self.px[i + 2] as u32, self.px[i + 1] as u32, self.px[i] as u32)
    }

    /// Shrinks by averaging (area sampling), which is all the blurred backdrop needs.
    pub fn shrink(&self, w: u32, h: u32) -> Image {
        let mut out = Image::new(w, h);
        if self.w == 0 || self.h == 0 {
            return out;
        }
        for y in 0..h {
            let (y0, y1) = (y * self.h / h, ((y + 1) * self.h / h).max(y * self.h / h + 1).min(self.h));
            for x in 0..w {
                let (x0, x1) = (x * self.w / w, ((x + 1) * self.w / w).max(x * self.w / w + 1).min(self.w));
                let mut sum = [0u64; 4];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let i = ((sy * self.w + sx) * 4) as usize;
                        for c in 0..4 {
                            sum[c] += self.px[i + c] as u64;
                        }
                    }
                }
                let n = ((x1 - x0) * (y1 - y0)).max(1) as u64;
                let i = ((y * w + x) * 4) as usize;
                for c in 0..4 {
                    out.px[i + c] = (sum[c] / n) as u8;
                }
            }
        }
        out
    }

    fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Image {
        let mut out = Image::new(w, h);
        for row in 0..h {
            let from = (((y + row) * self.w + x) * 4) as usize;
            let to = (row * w * 4) as usize;
            out.px[to..to + (w * 4) as usize].copy_from_slice(&self.px[from..from + (w * 4) as usize]);
        }
        out
    }

    /// Crops near-black bars (YouTube thumbnails often have them).
    pub fn trim_letterbox(self) -> Image {
        let (w, h) = (self.w, self.h);
        if w < 6 || h < 6 {
            return self;
        }
        let dark = |x: u32, y: u32| {
            let (r, g, b) = self.rgb(x, y);
            r + g + b <= 60
        };
        let dark_row = |y: u32| (0..w).step_by(2).all(|x| dark(x, y));
        let dark_col = |x: u32| (0..h).step_by(2).all(|y| dark(x, y));
        let (mut top, mut bottom, mut left, mut right) = (0, h - 1, 0, w - 1);
        while top < h / 3 && dark_row(top) {
            top += 1;
        }
        while bottom > h * 2 / 3 && dark_row(bottom) {
            bottom -= 1;
        }
        while left < w / 3 && dark_col(left) {
            left += 1;
        }
        while right > w * 2 / 3 && dark_col(right) {
            right -= 1;
        }
        if top < 3 && h - 1 - bottom < 3 && left < 3 && w - 1 - right < 3 {
            return self;
        }
        if top >= h / 3 || left >= w / 3 {
            return self; // mostly dark art: leave it alone
        }
        self.crop(left, top, right - left + 1, bottom - top + 1)
    }
}

pub const DEFAULT_ACCENT: u32 = 0xFF8AA2FF;

/// A vivid, readable colour that represents the artwork (0xAARRGGBB).
pub fn accent(art: &Image) -> u32 {
    if art.w == 0 || art.h == 0 {
        return DEFAULT_ACCENT;
    }
    let small = art.shrink(24, 24);
    // Bucket by hue; weight each pixel by how colourful and how bright-but-not-blown-out it is.
    let mut buckets = [[0f64; 4]; 24];
    for y in 0..24 {
        for x in 0..24 {
            let (r, g, b) = small.rgb(x, y);
            let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
            let (hue, sat, lum) = to_hsl(r, g, b);
            if sat < 0.18 || !(0.12..=0.92).contains(&lum) {
                continue;
            }
            let w = sat * sat * (1.0 - (lum - 0.5).abs() * 1.4);
            let k = &mut buckets[(hue / 15.0) as usize % 24];
            *k = [k[0] + w, k[1] + r * w, k[2] + g * w, k[3] + b * w];
        }
    }
    let best = buckets.iter().fold([0f64; 4], |best, k| if k[0] > best[0] { *k } else { best });
    if best[0] < 0.6 {
        return DEFAULT_ACCENT; // essentially grayscale art
    }
    let (h, s, l) = to_hsl(best[1] / best[0], best[2] / best[0], best[3] / best[0]);
    // Keep it vivid and bright enough to read on the dark card.
    let (r, g, b) = from_hsl(h, (s * 1.15).clamp(0.45, 0.9), l.clamp(0.58, 0.72));
    0xFF00_0000 | ((r * 255.0) as u32) << 16 | ((g * 255.0) as u32) << 8 | (b * 255.0) as u32
}

fn to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let part = |t: f64| {
        let t = (t % 360.0 + 360.0) % 360.0 / 360.0;
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (part(h + 120.0), part(h), part(h - 120.0))
}

/// A black soft shadow of a rounded rectangle `w`×`h` sitting `margin` pixels inside the image,
/// pushed down by `drop`. Computed once per size from the distance to the rectangle's edge.
pub fn shadow(w: u32, h: u32, margin: u32, radius: f32, blur: f32, drop: f32, opacity: f32) -> Image {
    let (iw, ih) = (w + margin * 2, h + margin * 2);
    let mut out = Image::new(iw, ih);
    let (cx, cy) = (iw as f32 / 2.0, ih as f32 / 2.0 + drop);
    let (hx, hy) = (w as f32 / 2.0 - radius, h as f32 / 2.0 - radius);
    for y in 0..ih {
        for x in 0..iw {
            let dx = ((x as f32 + 0.5 - cx).abs() - hx).max(0.0);
            let dy = ((y as f32 + 0.5 - cy).abs() - hy).max(0.0);
            let outside = (dx * dx + dy * dy).sqrt() - radius;
            // A smooth step from full inside the edge to nothing `blur` outside it.
            let t = ((outside + blur * 0.5) / blur).clamp(0.0, 1.0);
            let a = (1.0 - t * t * (3.0 - 2.0 * t)) * opacity;
            out.px[((y * iw + x) * 4 + 3) as usize] = (a * 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8) -> Image {
        let mut i = Image::new(w, h);
        for p in i.px.chunks_exact_mut(4) {
            p.copy_from_slice(&[b, g, r, 255]);
        }
        i
    }

    #[test]
    fn accent_follows_the_art() {
        let a = accent(&solid(64, 64, 200, 30, 30));
        assert!((a >> 16 & 255) > (a >> 8 & 255) + 40, "red art gives a red accent: {a:08X}");
        assert_eq!(accent(&solid(64, 64, 90, 90, 90)), DEFAULT_ACCENT);
        assert_eq!(accent(&Image::default()), DEFAULT_ACCENT);
    }

    #[test]
    fn letterbox_is_trimmed() {
        let mut i = solid(40, 40, 0, 0, 0);
        for y in 8..32 {
            for x in 0..40 {
                i.px[((y * 40 + x) * 4 + 2) as usize] = 200;
            }
        }
        let t = i.trim_letterbox();
        assert_eq!((t.w, t.h), (40, 24));
        let dark = solid(40, 40, 0, 0, 0).trim_letterbox();
        assert_eq!((dark.w, dark.h), (40, 40), "all-dark art is left alone");
    }

    #[test]
    fn shrink_averages_and_shadow_fades_out() {
        let s = solid(30, 20, 10, 20, 30).shrink(3, 2);
        assert_eq!(&s.px[..4], &[30, 20, 10, 255]);
        let sh = shadow(100, 60, 20, 12.0, 20.0, 0.0, 0.5);
        let alpha = |x: u32, y: u32| sh.px[((y * sh.w + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert!(alpha(70, 50) >= 126);
        assert!(alpha(70, 20) > 0 && alpha(70, 20) < 127, "the edge is soft");
    }
}
