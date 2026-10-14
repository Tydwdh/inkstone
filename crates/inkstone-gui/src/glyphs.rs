//! Glyph rasterization: turning a cell's grapheme into a rectangle of an atlas.
//!
//! The terminal frontend hands a string to the terminal and the terminal's font
//! machinery draws it. A window has to shape and rasterize itself, so this
//! module owns the one `FontSystem` (cosmic-text: shaping, font fallback, bidi)
//! and the one atlas every glyph in the session lives in.
//!
//! Shaping happens **per cell** rather than per line: a cell carries its own
//! weight and style, so a row of mixed attributes is a row of separate shaping
//! units anyway, and a cell is small enough that a `HashMap` hit skips the
//! shaping entirely on every frame after the first.
//!
//! The atlas is one `R8` texture — glyphs are masks, so one byte per texel —
//! filled on demand by a shelf packer that grows the texture when it runs out
//! of room. It is packed with a transparent border around every glyph so that
//! bilinear sampling at a glyph's edge reads that border instead of the
//! neighbouring glyph.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, PhysicalGlyph, Shaping,
    Style as FontStyle, SwashCache, SwashContent, Weight, Wrap,
};

/// The cell a row of glyphs is laid out on, in logical pixels: as wide as `M`
/// advances and as tall as the line height.
pub const TARGET_CELL: (f32, f32) = (16.0, 30.0);

/// The font size the advance of `M` is probed at before the real size is
/// derived from it.
const PROBE_FONT_SIZE: f32 = 16.0;

/// Transparent border kept around each glyph in the atlas, in texels.
const ATLAS_PADDING: u32 = 1;

/// The atlas starts this big and doubles when it runs out of room.
const INITIAL_ATLAS: (u32, u32) = (512, 512);

/// A rectangle of the glyph atlas, in texels.
///
/// The default (all zeroes) is the empty rectangle, which the renderer treats as
/// "nothing to draw".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasRect {
    /// Left edge, in texels from the atlas's left.
    pub x: u32,
    /// Top edge, in texels from the atlas's top.
    pub y: u32,
    /// Width, in texels.
    pub w: u32,
    /// Height, in texels.
    pub h: u32,
}

impl AtlasRect {
    /// Whether this rectangle holds anything to draw.
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// A rasterized glyph: where its bitmap is, and where that bitmap belongs
/// relative to the top-left corner of its cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    /// The bitmap's place in the atlas.
    pub rect: AtlasRect,
    /// The bitmap's top-left, in cell-local pixels (the glyph's ink usually
    /// starts above and right of the cell's corner).
    pub offset: (f32, f32),
}

/// The font, the atlas, and the two caches in front of them.
pub struct Glyphs {
    font_system: FontSystem,
    swash: SwashCache,
    /// Reused to shape one cell at a time.
    buffer: Buffer,
    cell: (f32, f32),
    atlas: Atlas,
    /// Rasterized glyphs, by everything that decides what a glyph looks like.
    rasterized: HashMap<CacheKey, Glyph>,
    /// Glyphs by what a cell holds, so a repeated character is not reshaped.
    shaped: HashMap<(char, bool, bool), Glyph>,
    /// Reused to copy a rasterized bitmap out of the swash cache.
    bitmap: Vec<u8>,
    /// Atlas rectangles filled since the last [`Glyphs::take_pending`].
    pending: Vec<AtlasRect>,
}

impl Glyphs {
    /// Load the system's fonts and size them so a cell is [`TARGET_CELL`] at
    /// `scale`.
    ///
    /// `scale` is the window's scale factor, so a HiDPI window gets glyphs
    /// rasterized at its real resolution rather than 1x glyphs stretched up.
    /// `max_dimension` is the device's `max_texture_dimension_2d`, which caps
    /// how far the atlas may grow.
    ///
    /// Fails when the system has no font that can draw a character at all, which
    /// is the one thing no amount of fallback can paper over.
    pub fn new(scale: f32, max_dimension: u32) -> Result<Self> {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let mut font_system = FontSystem::new();
        let regular = face(false, false);

        let probe_size = PROBE_FONT_SIZE * scale;
        let probe_advance = {
            let mut buffer =
                Buffer::new(&mut font_system, Metrics::new(probe_size, probe_size * 2.0));
            buffer.set_wrap(Wrap::None);
            buffer.set_size(None, None);
            measure(&mut buffer, &mut font_system, &regular).unwrap_or(0.0)
        };
        let font_size = size_for_cell(probe_size, probe_advance, TARGET_CELL.0 * scale)
            .ok_or_else(no_font_error)?;

        let metrics = Metrics::new(font_size, TARGET_CELL.1 * scale);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        let advance = measure(&mut buffer, &mut font_system, &regular).ok_or_else(no_font_error)?;

        Ok(Self {
            font_system,
            swash: SwashCache::new(),
            buffer,
            cell: cell_from(advance, metrics.line_height),
            atlas: Atlas::new(INITIAL_ATLAS.0, INITIAL_ATLAS.1, max_dimension),
            rasterized: HashMap::new(),
            shaped: HashMap::new(),
            bitmap: Vec::new(),
            pending: Vec::new(),
        })
    }

    /// The cell a glyph is drawn in, in physical pixels.
    pub fn cell_size(&self) -> (f32, f32) {
        self.cell
    }

    /// The atlas's size in texels.
    pub fn atlas_size(&self) -> (u32, u32) {
        (self.atlas.width, self.atlas.height)
    }

    /// The atlas's whole contents, one byte per texel, row-major.
    pub fn pixels(&self) -> &[u8] {
        &self.atlas.pixels
    }

    /// The atlas rectangles filled since this was last called.
    ///
    /// The caller uploads them, then uploads nothing until the next call.
    pub fn take_pending(&mut self) -> Vec<AtlasRect> {
        std::mem::take(&mut self.pending)
    }

    /// The glyph for `text` (one cell's grapheme) in the given face.
    ///
    /// `None` means there is nothing to draw: a blank cell, a character no
    /// installed font covers, or an atlas that cannot grow any further.
    pub fn glyph(&mut self, text: &str, bold: bool, italic: bool) -> Option<Glyph> {
        let mut chars = text.chars();
        let key = match (chars.next(), chars.next()) {
            (Some(c), None) => Some((c, bold, italic)),
            // A cluster — `e` plus a combining acute, an emoji ZWJ sequence —
            // has no single character to key on, so it is reshaped every time.
            // The atlas lookup below still keeps it from being rasterized twice.
            _ => None,
        };
        if let Some(key) = key {
            if let Some(glyph) = self.shaped.get(&key) {
                return Some(*glyph);
            }
        }

        let attrs = face(bold, italic);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);
        let physical = self.lay_out()?;
        let cache_key = physical.cache_key;

        let glyph = match self.rasterized.get(&cache_key) {
            Some(glyph) => *glyph,
            None => {
                let glyph = self.rasterize(physical)?;
                self.rasterized.insert(cache_key, glyph);
                glyph
            }
        };
        if let Some(key) = key {
            self.shaped.insert(key, glyph);
        }
        Some(glyph)
    }

    /// The one glyph the buffer's text laid out as, and where it goes.
    fn lay_out(&self) -> Option<PhysicalGlyph> {
        let mut found = None;
        for run in self.buffer.layout_runs() {
            if let Some(glyph) = run.glyphs.first() {
                found = Some(glyph.physical((0.0, run.line_y), 1.0));
                break;
            }
        }
        found
    }

    /// Rasterize `physical` into the atlas, growing it if it is full.
    fn rasterize(&mut self, physical: PhysicalGlyph) -> Option<Glyph> {
        // The rasterized image borrows the swash cache, so everything needed
        // from it is copied out before the atlas is touched.
        let mut raster = None;
        if let Some(image) = self
            .swash
            .get_image(&mut self.font_system, physical.cache_key)
        {
            if image.placement.width > 0 && image.placement.height > 0 && !image.data.is_empty() {
                self.bitmap.clear();
                self.bitmap.extend_from_slice(&image.data);
                raster = Some((
                    image.placement.width,
                    image.placement.height,
                    image.placement.left,
                    image.placement.top,
                    image.content,
                ));
            }
        }
        let (width, height, left, top, content) = raster?;

        let slot = self
            .atlas
            .allocate(width + 2 * ATLAS_PADDING, height + 2 * ATLAS_PADDING)?;
        let rect = AtlasRect {
            x: slot.x + ATLAS_PADDING,
            y: slot.y + ATLAS_PADDING,
            w: width,
            h: height,
        };
        blit(
            &mut self.atlas.pixels,
            self.atlas.width,
            rect,
            &self.bitmap,
            content,
        );
        self.pending.push(rect);
        Some(Glyph {
            rect,
            // A glyph is drawn above and right of its origin: the placement says
            // how far the ink starts from the pen, and y grows downward.
            offset: (
                physical.x as f32 + left as f32,
                physical.y as f32 - top as f32,
            ),
        })
    }
}

/// The attributes for one face: a monospace family at the wanted weight and
/// style.
///
/// `Monospace` is what the font database is asked for; when the system has no
/// such family, cosmic-text's fallback chain picks whatever it has that can draw
/// the character, which is why an unusual script still appears.
fn face(bold: bool, italic: bool) -> Attrs<'static> {
    Attrs::new()
        .family(Family::Monospace)
        .weight(if bold { Weight::BOLD } else { Weight::NORMAL })
        .style(if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        })
}

/// The advance width of `M` in `buffer`, or `None` when nothing was laid out —
/// which is what an empty font database looks like.
fn measure(buffer: &mut Buffer, font_system: &mut FontSystem, attrs: &Attrs) -> Option<f32> {
    buffer.set_text("M", attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    buffer.layout_runs().next().map(|run| run.line_w)
}

/// The font size that makes `M` advance `cell_width`, given what it advanced at
/// `probe_size`.
///
/// Advances are proportional to the font size, so one measurement is enough to
/// solve for the size that hits the target exactly, whatever font was chosen.
/// `None` when the probe produced no advance, which means no usable font.
fn size_for_cell(probe_size: f32, probe_advance: f32, cell_width: f32) -> Option<f32> {
    if !probe_advance.is_finite()
        || probe_advance <= 0.0
        || !cell_width.is_finite()
        || cell_width <= 0.0
    {
        return None;
    }
    Some(probe_size * cell_width / probe_advance)
}

/// The cell a row is laid out on: the advance of `M` wide, the line height tall.
///
/// Neither may be zero: the cell is what turns a window size into a grid size,
/// and a zero cell would be a division by zero there.
fn cell_from(advance: f32, line_height: f32) -> (f32, f32) {
    (advance.max(1.0), line_height.max(1.0))
}

/// What to tell a user whose system has no font.
fn no_font_error() -> anyhow::Error {
    anyhow!(
        "no usable font was found on this system; install a monospace font \
         (Consolas, DejaVu Sans Mono, Menlo, ...) and start inkstone-gui again"
    )
}

/// Copy a glyph's bitmap into the atlas at `rect`.
///
/// The atlas holds one byte per texel, so a colour bitmap (an emoji) contributes
/// its alpha channel: the glyph keeps its shape and loses its colours.
fn blit(pixels: &mut [u8], atlas_width: u32, rect: AtlasRect, data: &[u8], content: SwashContent) {
    let stride = match content {
        SwashContent::Mask => 1,
        SwashContent::Color | SwashContent::SubpixelMask => 4,
    };
    let atlas_width = atlas_width as usize;
    let width = rect.w as usize;
    for row in 0..rect.h as usize {
        let source = row * width * stride;
        let destination = (rect.y as usize + row) * atlas_width + rect.x as usize;
        if source + width * stride > data.len() || destination + width > pixels.len() {
            return;
        }
        for column in 0..width {
            // The last byte of a colour texel is its alpha, and the only byte of
            // a mask texel is its coverage.
            pixels[destination + column] = data[source + column * stride + stride - 1];
        }
    }
}

/// A shelf-packed `R8` atlas of rasterized glyphs.
struct Atlas {
    width: u32,
    height: u32,
    /// The largest the atlas is allowed to get; the texture limit of the device.
    max_dimension: u32,
    /// One byte per texel, row-major.
    pixels: Vec<u8>,
    /// The shelves, top to bottom, each holding a row of glyphs.
    shelves: Vec<Shelf>,
}

/// One horizontal band of the atlas: where it is, and how far along it the next
/// glyph goes.
struct Shelf {
    y: u32,
    x: u32,
    height: u32,
}

impl Atlas {
    /// An empty atlas with room to start with.
    fn new(width: u32, height: u32, max_dimension: u32) -> Self {
        Self {
            width,
            height,
            max_dimension,
            pixels: vec![0; (width as usize) * (height as usize)],
            shelves: Vec::new(),
        }
    }

    /// Space for a `w` by `h` glyph, opening a shelf or growing the atlas when
    /// the existing shelves do not have it.
    ///
    /// `None` when the glyph is wider than the atlas can ever be, or when the
    /// atlas is already at the device's texture limit.
    fn allocate(&mut self, w: u32, h: u32) -> Option<AtlasRect> {
        if w == 0 || h == 0 || w > self.width || h > self.max_dimension {
            return None;
        }
        for shelf in &mut self.shelves {
            if shelf.height >= h && shelf.x + w <= self.width {
                let rect = AtlasRect {
                    x: shelf.x,
                    y: shelf.y,
                    w,
                    h,
                };
                shelf.x += w;
                return Some(rect);
            }
        }

        let y = match self.shelves.last() {
            Some(shelf) => shelf.y + shelf.height,
            None => 0,
        };
        if !self.ensure_height(y + h) {
            return None;
        }
        self.shelves.push(Shelf { y, x: w, height: h });
        Some(AtlasRect { x: 0, y, w, h })
    }

    /// Grow the atlas so it is at least `required` texels tall, copying what is
    /// already packed into the new buffer. Returns whether it fits.
    fn ensure_height(&mut self, required: u32) -> bool {
        if required <= self.height {
            return true;
        }
        let grown = self
            .height
            .saturating_mul(2)
            .max(required)
            .min(self.max_dimension);
        if grown < required {
            return false;
        }

        let stride = self.width as usize;
        let mut pixels = vec![0; stride * (grown as usize)];
        for row in 0..self.height as usize {
            let start = row * stride;
            let end = start + stride;
            if let (Some(source), Some(destination)) =
                (self.pixels.get(start..end), pixels.get_mut(start..end))
            {
                destination.copy_from_slice(source);
            }
        }
        self.pixels = pixels;
        self.height = grown;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_font_is_sized_from_one_advance_measurement() {
        // A font whose `M` is 0.6 of the font size: a 16px cell wants 26.67px of
        // font. The exact number does not matter, the ratio does.
        let size = size_for_cell(16.0, 9.6, 16.0).expect("a probe with an advance has a size");
        assert!((size - 26.667).abs() < 0.01, "{size}");

        // A wider advance needs a smaller font to fill the same cell.
        let smaller = size_for_cell(16.0, 12.0, 16.0).expect("still solvable");
        assert!(smaller < size, "{smaller} < {size}");
    }

    #[test]
    fn a_font_that_lays_nothing_out_has_no_size() {
        assert!(size_for_cell(16.0, 0.0, 16.0).is_none());
        assert!(size_for_cell(16.0, -1.0, 16.0).is_none());
        assert!(size_for_cell(16.0, 9.6, 0.0).is_none());
    }

    #[test]
    fn a_cell_is_never_zero_sized() {
        assert_eq!(cell_from(9.6, 22.0), (9.6, 22.0));
        let (width, height) = cell_from(0.0, 0.0);
        assert!(width >= 1.0 && height >= 1.0, "{width}x{height}");
    }

    #[test]
    fn glyphs_are_packed_side_by_side_then_shelf_by_shelf() {
        let mut atlas = Atlas::new(64, 32, 128);

        let first = atlas.allocate(10, 8).expect("room for the first glyph");
        let second = atlas.allocate(12, 8).expect("room next to it");
        assert_eq!(first.y, second.y, "the second glyph shares the shelf");
        assert_eq!(second.x, first.x + first.w, "and starts where it ended");

        // Taller than the shelf it was opened against: a new shelf below it.
        let third = atlas.allocate(10, 12).expect("a taller glyph");
        assert!(
            third.y >= first.y + first.h,
            "{third:?} is below the first shelf"
        );
    }

    #[test]
    fn the_atlas_grows_when_it_runs_out_of_height() {
        let mut atlas = Atlas::new(16, 8, 64);
        let mut placed = Vec::new();
        for _ in 0..4 {
            placed.push(atlas.allocate(8, 8).expect("allocate"));
        }
        assert!(atlas.height > 8, "the atlas grew: {}", atlas.height);
        assert_eq!(
            atlas.pixels.len(),
            atlas.width as usize * atlas.height as usize
        );

        // Every rectangle is still inside the atlas after growing.
        for rect in placed {
            assert!(rect.x + rect.w <= atlas.width, "{rect:?}");
            assert!(rect.y + rect.h <= atlas.height, "{rect:?}");
        }
    }

    #[test]
    fn growth_stops_at_the_device_limit() {
        let mut atlas = Atlas::new(8, 8, 16);
        assert!(atlas.allocate(8, 8).is_some());
        assert!(
            atlas.allocate(8, 8).is_some(),
            "the second shelf fits at 16"
        );
        assert_eq!(
            atlas.height, 16,
            "the atlas grew to the limit and no further"
        );
        assert!(
            atlas.allocate(8, 8).is_none(),
            "and then it refuses rather than overgrowing"
        );
    }

    #[test]
    fn a_glyph_wider_than_the_atlas_is_refused() {
        let mut atlas = Atlas::new(8, 8, 64);
        assert!(atlas.allocate(9, 1).is_none());
        assert!(atlas.allocate(0, 1).is_none());
        assert!(atlas.allocate(1, 0).is_none());
    }

    #[test]
    fn a_colour_bitmap_contributes_its_alpha() {
        let mut pixels = vec![0u8; 16];
        let rect = AtlasRect {
            x: 1,
            y: 1,
            w: 2,
            h: 1,
        };
        // Two RGBA texels: an opaque one, then a transparent one.
        let data = [255, 0, 0, 255, 0, 0, 255, 0];
        blit(&mut pixels, 4, rect, &data, SwashContent::Color);
        assert_eq!(pixels[4 + 1], 255, "the first texel's alpha");
        assert_eq!(pixels[4 + 2], 0, "the second texel's alpha");
    }

    #[test]
    fn a_mask_bitmap_is_copied_byte_for_byte() {
        let mut pixels = vec![0u8; 16];
        let rect = AtlasRect {
            x: 0,
            y: 0,
            w: 3,
            h: 1,
        };
        blit(&mut pixels, 4, rect, &[10, 20, 30], SwashContent::Mask);
        assert_eq!(&pixels[..3], &[10, 20, 30]);
    }

    #[test]
    fn a_bitmap_that_does_not_fit_is_dropped_rather_than_panicking() {
        let mut pixels = vec![0u8; 4];
        let rect = AtlasRect {
            x: 3,
            y: 0,
            w: 4,
            h: 1,
        };
        blit(&mut pixels, 4, rect, &[1, 2, 3, 4], SwashContent::Mask);
        assert!(pixels.iter().all(|&byte| byte == 0));
    }
}
