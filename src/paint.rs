//! Drawing on the CPU into the window's pixel buffer: rounded rectangles with borders, gradients
//! and soft shadows, strokes, and text. Every shape is anti-aliased from its signed distance, so
//! nothing needs a GPU, and the only memory a frame takes is the window's own buffer.

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, VariableFont, point};
use std::collections::HashMap;

/// A colour with straight alpha, each part from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rgba {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

pub(crate) fn rgb(hex: u32) -> Rgba {
    let part = |shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
    Rgba { r: part(16), g: part(8), b: part(0), a: 1.0 }
}

pub(crate) const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
pub(crate) const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

impl Rgba {
    pub fn alpha(self, a: f32) -> Self {
        Self { a: self.a * a, ..self }
    }

    fn mix(self, other: Self, t: f32) -> Self {
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        Self { r: lerp(self.r, other.r), g: lerp(self.g, other.g), b: lerp(self.b, other.b), a: lerp(self.a, other.a) }
    }
}

/// A rectangle in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn inset(&self, by: f32) -> Self {
        Self::new(self.x + by, self.y + by, (self.w - 2.0 * by).max(0.0), (self.h - 2.0 * by).max(0.0))
    }

    fn intersect(&self, other: &Self) -> Self {
        let (x, y) = (self.x.max(other.x), self.y.max(other.y));
        Self::new(x, y, (self.right().min(other.right()) - x).max(0.0), (self.bottom().min(other.bottom()) - y).max(0.0))
    }
}

/// How a shape is filled: one colour, or a gradient from its top edge to its bottom edge.
#[derive(Clone, Copy)]
pub(crate) enum Fill {
    Solid(Rgba),
    Vertical(Rgba, Rgba),
}

impl From<Rgba> for Fill {
    fn from(color: Rgba) -> Self {
        Fill::Solid(color)
    }
}

/// The window's pixels, `0x00RRGGBB`, and the rectangle drawing is clipped to.
pub(crate) struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: usize,
    height: usize,
    clip: Rect,
}

/// The signed distance from a point to a rounded rectangle: negative inside.
fn rounded_distance(rect: &Rect, radius: f32, x: f32, y: f32) -> f32 {
    let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0).max(0.0);
    let (hw, hh) = (rect.w / 2.0, rect.h / 2.0);
    let qx = (x - rect.x - hw).abs() - hw + radius;
    let qy = (y - rect.y - hh).abs() - hh + radius;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// The complementary error function's half, for a shadow's edge: 1 well inside, 0 well outside.
fn soft_edge(distance: f32, sigma: f32) -> f32 {
    let x = distance / (sigma * std::f32::consts::SQRT_2);
    // Abramowitz and Stegun 7.1.26, good to about 1e-7.
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let poly = t * (0.254_829_6 + t * (-0.284_496_7 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    let erf = 1.0 - poly * (-x * x).exp();
    0.5 * (1.0 - erf.copysign(x))
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Self {
        Self { pixels, width, height, clip: Rect::new(0.0, 0.0, width as f32, height as f32) }
    }

    pub fn set_clip(&mut self, clip: Option<Rect>) {
        let full = Rect::new(0.0, 0.0, self.width as f32, self.height as f32);
        self.clip = clip.map_or(full, |clip| clip.intersect(&full));
    }

    pub fn clear(&mut self, color: Rgba) {
        self.pixels.fill(pack(color));
    }

    /// The pixel rows and columns `area` touches, inside the clip.
    fn span(&self, area: Rect) -> (usize, usize, usize, usize) {
        let area = area.intersect(&self.clip);
        let x0 = area.x.floor().max(0.0) as usize;
        let y0 = area.y.floor().max(0.0) as usize;
        let x1 = (area.right().ceil().max(0.0) as usize).min(self.width);
        let y1 = (area.bottom().ceil().max(0.0) as usize).min(self.height);
        (x0, y0, x1, y1)
    }

    /// Covers pixel `x, y` with `color` at `coverage`, also fading the parts of edge pixels the
    /// clip cuts off.
    fn blend(&mut self, x: usize, y: usize, color: Rgba, coverage: f32) {
        let clip = &self.clip;
        let cut = (clip.right() - x as f32).clamp(0.0, 1.0).min(x as f32 + 1.0 - clip.x).min((clip.bottom() - y as f32).clamp(0.0, 1.0)).min(y as f32 + 1.0 - clip.y);
        let alpha = color.a * coverage * cut.clamp(0.0, 1.0);
        if alpha <= 0.002 {
            return;
        }
        let at = y * self.width + x;
        let old = self.pixels[at];
        let mix = |shift: u32, part: f32| {
            let below = ((old >> shift) & 0xFF) as f32;
            ((below + (part * 255.0 - below) * alpha).round().clamp(0.0, 255.0) as u32) << shift
        };
        self.pixels[at] = mix(16, color.r) | mix(8, color.g) | mix(0, color.b);
    }

    /// A rounded rectangle.
    pub fn round_rect(&mut self, rect: Rect, radius: f32, fill: impl Into<Fill>) {
        let fill = fill.into();
        let (x0, y0, x1, y1) = self.span(rect);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let color = match fill {
                Fill::Solid(color) => color,
                Fill::Vertical(top, bottom) => top.mix(bottom, ((py - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0)),
            };
            for x in x0..x1 {
                let coverage = (0.5 - rounded_distance(&rect, radius, x as f32 + 0.5, py)).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend(x, y, color, coverage);
                }
            }
        }
    }

    pub fn rect(&mut self, rect: Rect, color: Rgba) {
        self.round_rect(rect, 0.0, color);
    }

    /// A border `width` wide, drawn inside a rounded rectangle's edge.
    pub fn border(&mut self, rect: Rect, radius: f32, width: f32, color: Rgba) {
        let inner = rect.inset(width);
        let inner_radius = (radius - width).max(0.0);
        let (x0, y0, x1, y1) = self.span(rect);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let outer = (0.5 - rounded_distance(&rect, radius, px, py)).clamp(0.0, 1.0);
                let hole = (0.5 - rounded_distance(&inner, inner_radius, px, py)).clamp(0.0, 1.0);
                if outer > hole {
                    self.blend(x, y, color, outer - hole);
                }
            }
        }
    }

    /// A soft shadow of a rounded rectangle, moved down by `dy` and grown by `spread`.
    pub fn shadow(&mut self, rect: Rect, radius: f32, blur: f32, spread: f32, dy: f32, color: Rgba) {
        let shape = Rect::new(rect.x - spread, rect.y + dy - spread, rect.w + 2.0 * spread, rect.h + 2.0 * spread);
        let radius = radius + spread;
        let sigma = (blur / 2.0).max(0.01);
        let (x0, y0, x1, y1) = self.span(shape.inset(-3.0 * sigma - 1.0));
        for y in y0..y1 {
            for x in x0..x1 {
                let distance = rounded_distance(&shape, radius, x as f32 + 0.5, y as f32 + 0.5);
                self.blend(x, y, color, soft_edge(distance, sigma));
            }
        }
    }

    /// A stroke `width` wide with flat ends, along points `width / 2` apart or closer.
    pub fn stroke(&mut self, points: &[(f32, f32)], width: f32, color: Rgba) {
        let half = width / 2.0;
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in points {
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
        }
        let (x0, y0, x1, y1) = self.span(Rect::new(left - width, top - width, right - left + 2.0 * width, bottom - top + 2.0 * width));
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let segments = points.len() - 1;
                let mut coverage: f32 = 0.0;
                for (index, pair) in points.windows(2).enumerate() {
                    let ((ax, ay), (bx, by)) = (pair[0], pair[1]);
                    let (dx, dy) = (bx - ax, by - ay);
                    let length = (dx * dx + dy * dy).sqrt().max(1e-6);
                    let t = ((px - ax) * dx + (py - ay) * dy) / (length * length);
                    // Past either end of the whole line, the stroke stops flat.
                    let beyond = if index == 0 && t < 0.0 {
                        -t * length
                    } else if index + 1 == segments && t > 1.0 {
                        (t - 1.0) * length
                    } else {
                        0.0
                    };
                    let (cx, cy) = (ax + dx * t.clamp(0.0, 1.0), ay + dy * t.clamp(0.0, 1.0));
                    let across = if beyond > 0.0 { ((px - ax) * dy - (py - ay) * dx).abs() / length } else { (px - cx).hypot(py - cy) };
                    coverage = coverage.max((half + 0.5 - across).clamp(0.0, 1.0) * (0.5 - beyond).clamp(0.0, 1.0).max(if beyond > 0.0 { 0.0 } else { 1.0 }));
                }
                if coverage > 0.0 {
                    self.blend(x, y, color, coverage);
                }
            }
        }
    }
}

fn pack(color: Rgba) -> u32 {
    let part = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    part(color.r) << 16 | part(color.g) << 8 | part(color.b)
}

/// One rasterized glyph: its coverage, and where its top-left corner sits from the pen.
struct Bitmap {
    left: i32,
    top: i32,
    width: usize,
    coverage: Vec<u8>,
}

/// The fonts text draws with, and the glyphs drawn so far. It's dropped with the window, so a
/// mixer closed to the tray holds none of it.
pub(crate) struct Text {
    /// The interface font, then fonts found for characters it doesn't have.
    fonts: Vec<FontVec>,
    /// Which font draws each character, as `fonts` indices.
    chosen: HashMap<char, usize>,
    advances: HashMap<(char, u32), f32>,
    glyphs: HashMap<(usize, GlyphId, u32, u8), Option<Bitmap>>,
}

/// Steps between whole pixels a glyph can start at.
const SUBPIXELS: f32 = 4.0;

/// The interface font: Inter, which GNOME and KDE ship as Adwaita Sans.
const FAMILIES: &str = "Inter Variable,Inter,Adwaita Sans,sans-serif";

/// The font file fontconfig picks for `pattern`, at its regular weight.
fn find_font(pattern: &str) -> Option<FontVec> {
    let output = std::process::Command::new("fc-match").args(["--format=%{file}\n%{index}", pattern]).output().ok()?;
    let found = String::from_utf8(output.stdout).ok()?;
    let mut lines = found.lines();
    let path = lines.next()?;
    let index = lines.next().and_then(|index| index.parse().ok()).unwrap_or(0);
    regular(std::fs::read(path).ok()?, index)
}

/// A font from its file, at its regular weight if it has a weight axis.
fn regular(data: Vec<u8>, index: u32) -> Option<FontVec> {
    let mut font = FontVec::try_from_vec_and_index(data, index).ok()?;
    font.set_variation(b"wght", 400.0);
    Some(font)
}

impl Text {
    pub fn load() -> Self {
        let font = find_font(FAMILIES).or_else(|| regular(std::fs::read("/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf").ok()?, 0));
        if font.is_none() {
            eprintln!("Couldn't find a font (fc-match is part of fontconfig); the window shows no text.");
        }
        Self { fonts: font.into_iter().collect(), chosen: HashMap::new(), advances: HashMap::new(), glyphs: HashMap::new() }
    }

    /// The font that draws `c`, looking one up the first time the interface font lacks it.
    fn font_for(&mut self, c: char) -> Option<usize> {
        if let Some(&index) = self.chosen.get(&c) {
            return Some(index);
        }
        let mut index = self.fonts.iter().position(|font| font.glyph_id(c).0 != 0);
        if index.is_none()
            && !self.fonts.is_empty()
            && let Some(font) = find_font(&format!("{FAMILIES}:charset={:x}", c as u32)).filter(|font| font.glyph_id(c).0 != 0)
        {
            self.fonts.push(font);
            index = Some(self.fonts.len() - 1);
        }
        let index = index.or(if self.fonts.is_empty() { None } else { Some(0) })?;
        self.chosen.insert(c, index);
        Some(index)
    }

    /// The scale that makes a font's em `size` pixels.
    fn scale(font: &FontVec, size: f32) -> PxScale {
        PxScale::from(size * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0))
    }

    /// How far above and below the baseline the interface font reaches at `size`.
    pub fn metrics(&self, size: f32) -> (f32, f32) {
        let Some(font) = self.fonts.first() else { return (size * 0.8, size * 0.2) };
        let scaled = font.as_scaled(Self::scale(font, size));
        (scaled.ascent(), -scaled.descent())
    }

    fn advance(&mut self, c: char, size: f32) -> f32 {
        let key = (c, size.to_bits());
        if let Some(&advance) = self.advances.get(&key) {
            return advance;
        }
        let Some(index) = self.font_for(c) else { return 0.0 };
        let font = &self.fonts[index];
        let advance = font.as_scaled(Self::scale(font, size)).h_advance(font.glyph_id(c));
        self.advances.insert(key, advance);
        advance
    }

    /// Each character's advance; with `tabular`, every digit takes a zero's.
    fn advances(&mut self, text: &str, size: f32, tabular: bool) -> Vec<(char, f32)> {
        text.chars()
            .map(|c| {
                let shown = if tabular && c.is_ascii_digit() { '0' } else { c };
                (c, self.advance(shown, size))
            })
            .collect()
    }

    pub fn width(&mut self, text: &str, size: f32, tabular: bool) -> f32 {
        self.advances(text, size, tabular).iter().map(|(_, advance)| advance).sum()
    }

    /// `text`, cut short with an ellipsis if it's wider than `max`.
    pub fn fit(&mut self, text: &str, size: f32, max: f32) -> String {
        if self.width(text, size, false) <= max {
            return text.to_owned();
        }
        let budget = max - self.width("…", size, false);
        let mut used = 0.0;
        let mut kept = String::new();
        for (c, advance) in self.advances(text, size, false) {
            used += advance;
            if used > budget {
                break;
            }
            kept.push(c);
        }
        kept.truncate(kept.trim_end().len());
        kept + "…"
    }

    /// Draws `text` from `x` along `baseline`, returning where it ends.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, canvas: &mut Canvas, text: &str, x: f32, baseline: f32, size: f32, color: Rgba, tabular: bool) -> f32 {
        let mut pen = x;
        for (c, advance) in self.advances(text, size, tabular) {
            let offset = if tabular && c.is_ascii_digit() { (advance - self.advance(c, size)) / 2.0 } else { 0.0 };
            self.glyph(canvas, c, pen + offset, baseline, size, color);
            pen += advance;
        }
        pen
    }

    fn glyph(&mut self, canvas: &mut Canvas, c: char, x: f32, baseline: f32, size: f32, color: Rgba) {
        if c.is_whitespace() {
            return;
        }
        let Some(index) = self.font_for(c) else { return };
        let whole = x.floor();
        let step = ((x - whole) * SUBPIXELS).floor() as u8;
        let font = &self.fonts[index];
        let id = font.glyph_id(c);
        let bitmap = self.glyphs.entry((index, id, size.to_bits(), step)).or_insert_with(|| {
            let glyph = id.with_scale_and_position(Self::scale(font, size), point(f32::from(step) / SUBPIXELS, 0.0));
            font.outline_glyph(glyph).map(|outline| {
                let bounds = outline.px_bounds();
                let width = bounds.width() as usize;
                let mut coverage = vec![0u8; width * bounds.height() as usize];
                outline.draw(|gx, gy, value| coverage[gy as usize * width + gx as usize] = (value.clamp(0.0, 1.0) * 255.0) as u8);
                Bitmap { left: bounds.min.x as i32, top: bounds.min.y as i32, width, coverage }
            })
        });
        let Some(bitmap) = bitmap else { return };
        let (left, top) = (whole as i32 + bitmap.left, baseline.round() as i32 + bitmap.top);
        for (row, line) in bitmap.coverage.chunks(bitmap.width.max(1)).enumerate() {
            for (column, &value) in line.iter().enumerate() {
                let (px, py) = (left + column as i32, top + row as i32);
                if value > 0 && px >= 0 && py >= 0 && (px as usize) < canvas.width && (py as usize) < canvas.height {
                    canvas.blend(px as usize, py as usize, color, f32::from(value) / 255.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_cover_their_inside_and_fade_at_the_edge() {
        let mut pixels = vec![0; 20 * 20];
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.round_rect(Rect::new(2.0, 2.0, 10.0, 10.0), 0.0, WHITE);
        canvas.round_rect(Rect::new(14.5, 2.0, 3.0, 3.0), 0.0, WHITE);
        assert_eq!(pixels[5 * 20 + 5], 0xFFFFFF);
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[3 * 20 + 14] & 0xFF, 128);
    }

    #[test]
    fn a_shadow_is_half_strength_at_its_edge() {
        assert!((soft_edge(0.0, 2.0) - 0.5).abs() < 1e-4);
        assert!(soft_edge(-10.0, 2.0) > 0.99);
        assert!(soft_edge(10.0, 2.0) < 0.01);
    }
}
