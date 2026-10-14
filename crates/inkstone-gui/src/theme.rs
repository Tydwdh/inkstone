//! The window theme: turning a protocol [`Color`] into a pixel colour.
//!
//! A terminal frontend hands a colour to the terminal and the terminal decides
//! what it looks like. A window has to decide itself, so this module is the
//! other half of the protocol's colour model: the default foreground and
//! background, the 256-colour palette, and what the attribute bits do to a cell.
//!
//! The palette is *computed* — 16 named colours, then the 6×6×6 cube, then the
//! 24-step grey ramp, which is exactly how a 256-colour terminal maps an index
//! to an RGB triple. Pasting the resulting table would be 256 chances to make a
//! typo in a value no test can derive from anything.

use inkstone_rpc::{
    Cell, Color, ATTR_BOLD, ATTR_FAINT, ATTR_ITALIC, ATTR_REVERSE, ATTR_STRIKETHROUGH,
    ATTR_UNDERLINE,
};

/// An RGBA colour, one `f32` per channel, in the `0.0..=1.0` range the GPU wants.
pub type Rgba = [f32; 4];

/// The window's background: what `Color::Reset` means for a cell's background.
pub const BACKGROUND: Rgba = [0.09, 0.10, 0.12, 1.0];

/// The window's foreground: what `Color::Reset` means for a cell's foreground.
pub const FOREGROUND: Rgba = [0.86, 0.88, 0.91, 1.0];

/// How far a faint cell's foreground is blended toward its background.
///
/// A terminal implements "dim" however its font and renderer make convenient; in
/// a window the honest reading is "halfway to the background", which is what
/// most GPU terminals do.
pub const FAINT: f32 = 0.5;

/// The sixteen named colours, in the order the protocol's indexes use.
///
/// These are xterm's defaults, which are what every terminal's 256-colour mode
/// stands in for. They are the one part of the palette that is not arithmetic:
/// the cube and the grey ramp below are computed from them.
const NAMED: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // 0 black
    [0xcd, 0x00, 0x00], // 1 red
    [0x00, 0xcd, 0x00], // 2 green
    [0xcd, 0xcd, 0x00], // 3 yellow
    [0x00, 0x00, 0xee], // 4 blue
    [0xcd, 0x00, 0xcd], // 5 magenta
    [0x00, 0xcd, 0xcd], // 6 cyan
    [0xe5, 0xe5, 0xe5], // 7 white
    [0x7f, 0x7f, 0x7f], // 8 bright black
    [0xff, 0x00, 0x00], // 9 bright red
    [0x00, 0xff, 0x00], // 10 bright green
    [0xff, 0xff, 0x00], // 11 bright yellow
    [0x5c, 0x5c, 0xff], // 12 bright blue
    [0xff, 0x00, 0xff], // 13 bright magenta
    [0x00, 0xff, 0xff], // 14 bright cyan
    [0xff, 0xff, 0xff], // 15 bright white
];

/// The six levels each channel of the colour cube steps through.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The 256 colours a `Color::Indexed(i)` can name, as RGB triples.
///
/// Indexes 0..16 are [`NAMED`], 16..232 are the 6×6×6 cube, and 232..256 are the
/// 24-step grey ramp — the layout every terminal agrees on.
pub const PALETTE: [[u8; 3]; 256] = palette();

/// Build [`PALETTE`] at compile time, from the three rules that define it.
const fn palette() -> [[u8; 3]; 256] {
    let mut table = [[0u8; 3]; 256];

    let mut i = 0;
    while i < NAMED.len() {
        table[i] = NAMED[i];
        i += 1;
    }

    let mut r = 0;
    while r < CUBE_LEVELS.len() {
        let mut g = 0;
        while g < CUBE_LEVELS.len() {
            let mut b = 0;
            while b < CUBE_LEVELS.len() {
                table[16 + r * 36 + g * 6 + b] = [CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]];
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }

    let mut grey = 0;
    while grey < 24 {
        // 232..256 step 8, 18, 28, ... — 8 + 10 * n.
        let level = 8 + (grey as u8) * 10;
        table[232 + grey] = [level, level, level];
        grey += 1;
    }

    table
}

/// How one cell should be painted: two colours and the four things an attribute
/// bit does to a glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// The colour the glyph (and any rule drawn with it) is painted in.
    pub fg: Rgba,
    /// The colour behind the glyph.
    pub bg: Rgba,
    /// Draw the bold face.
    pub bold: bool,
    /// Draw the italic face.
    pub italic: bool,
    /// Draw a rule under the glyph.
    pub underline: bool,
    /// Draw a rule through the glyph.
    pub strikethrough: bool,
}

/// Resolve a cell's colours and attributes into something drawable.
///
/// `Reset` is not a colour on its own — it means "whatever the window's theme
/// says" — so it resolves to [`FOREGROUND`] or [`BACKGROUND`] by which side of
/// the cell it is on. Reverse video swaps the two resolved colours (not the
/// unresolved ones: reversing `Reset` on `Reset` has to give the swapped theme),
/// and a faint cell is blended toward its background, which is what "dim" means
/// once the terminal is not doing it for you.
pub fn style(cell: &Cell) -> Style {
    let mut fg = resolve(cell.fg, FOREGROUND);
    let mut bg = resolve(cell.bg, BACKGROUND);
    if cell.attrs & ATTR_REVERSE != 0 {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.attrs & ATTR_FAINT != 0 {
        fg = blend(fg, bg, FAINT);
    }
    Style {
        fg,
        bg,
        bold: cell.attrs & ATTR_BOLD != 0,
        italic: cell.attrs & ATTR_ITALIC != 0,
        underline: cell.attrs & ATTR_UNDERLINE != 0,
        strikethrough: cell.attrs & ATTR_STRIKETHROUGH != 0,
    }
}

/// The pixel colour a protocol colour means, with `default` standing in for
/// `Reset`.
pub fn resolve(color: Color, default: Rgba) -> Rgba {
    match color {
        Color::Reset => default,
        Color::Indexed(index) => from_rgb8(PALETTE[index as usize]),
        Color::Rgb(r, g, b) => from_rgb8([r, g, b]),
    }
}

/// An 8-bit-per-channel colour as the GPU's `0.0..=1.0` form.
fn from_rgb8(rgb: [u8; 3]) -> Rgba {
    [
        f32::from(rgb[0]) / 255.0,
        f32::from(rgb[1]) / 255.0,
        f32::from(rgb[2]) / 255.0,
        1.0,
    ]
}

/// `amount` of the way from `from` to `to`.
fn blend(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let mut out = from;
    let mut channel = 0;
    while channel < 4 {
        out[channel] = from[channel] + (to[channel] - from[channel]) * amount;
        channel += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_rpc::Color;

    #[test]
    fn the_palette_has_the_three_regions_a_terminal_expects() {
        assert_eq!(PALETTE[0], [0, 0, 0], "index 0 is black");
        assert_eq!(PALETTE[7], [0xe5, 0xe5, 0xe5], "index 7 is the named white");
        assert_eq!(PALETTE[15], [0xff, 0xff, 0xff], "index 15 is bright white");
        assert_eq!(PALETTE[16], [0, 0, 0], "the cube starts at black");
        assert_eq!(PALETTE[196], [255, 0, 0], "196 is the cube's pure red");
        assert_eq!(PALETTE[21], [0, 0, 255], "21 is the cube's pure blue");
        assert_eq!(PALETTE[231], [255, 255, 255], "231 is the cube's white");
        assert_eq!(PALETTE[232], [8, 8, 8], "the grey ramp starts at 8");
        assert_eq!(PALETTE[255], [238, 238, 238], "and ends at 238");
    }

    #[test]
    fn the_cube_steps_each_channel_through_the_same_six_levels() {
        // Any two cube entries differing only in one channel differ by a step
        // of the level list, which is what makes the cube a cube.
        for (index, expected) in [(16, [0, 0, 0]), (17, [0, 0, 95]), (21, [0, 0, 255])] {
            assert_eq!(PALETTE[index], expected, "cube index {index}");
        }
        assert_eq!(PALETTE[16 + 36], [95, 0, 0], "red steps once per 36");
    }

    #[test]
    fn the_grey_ramp_is_monotonic() {
        let mut previous = PALETTE[232];
        for (index, grey) in PALETTE.iter().enumerate().skip(233) {
            assert_eq!(grey[0], grey[1], "index {index} is a grey");
            assert_eq!(grey[1], grey[2], "index {index} is a grey");
            assert!(
                grey[0] > previous[0],
                "index {index} is lighter than the one before it"
            );
            previous = *grey;
        }
    }

    #[test]
    fn reset_takes_the_side_of_the_cell_it_is_on() {
        assert_eq!(resolve(Color::Reset, FOREGROUND), FOREGROUND);
        assert_eq!(resolve(Color::Reset, BACKGROUND), BACKGROUND);
    }

    #[test]
    fn indexed_and_true_colour_resolve_to_their_rgb() {
        assert_eq!(
            resolve(Color::Indexed(1), BACKGROUND),
            from_rgb8([0xcd, 0, 0])
        );
        assert_eq!(
            resolve(Color::Rgb(1, 2, 3), BACKGROUND),
            from_rgb8([1, 2, 3])
        );
        // The last palette entry is reachable, so the index is not clamped or
        // stepped on the way in.
        assert_eq!(
            resolve(Color::Indexed(255), BACKGROUND),
            from_rgb8([238; 3])
        );
    }

    #[test]
    fn reverse_video_swaps_the_resolved_colours() {
        let cell = Cell::new("x")
            .with_fg(Color::Rgb(1, 2, 3))
            .with_attrs(ATTR_REVERSE);
        let style = style(&cell);
        assert_eq!(style.fg, BACKGROUND, "the background moved to the front");
        assert_eq!(style.bg, from_rgb8([1, 2, 3]), "and the front to the back");
    }

    #[test]
    fn faint_blends_toward_the_background() {
        let cell = Cell::new("x")
            .with_fg(Color::Rgb(0, 0, 0))
            .with_attrs(ATTR_FAINT);
        let style = style(&cell);
        let expected = blend(from_rgb8([0, 0, 0]), BACKGROUND, FAINT);
        assert_eq!(style.fg, expected);
        assert_ne!(style.fg, from_rgb8([0, 0, 0]), "it is dimmer than it was");
    }

    #[test]
    fn every_attribute_bit_picks_its_face_or_rule() {
        let plain = style(&Cell::new("x"));
        assert!(!plain.bold && !plain.italic && !plain.underline && !plain.strikethrough);

        assert!(style(&Cell::new("x").with_attrs(ATTR_BOLD)).bold);
        assert!(style(&Cell::new("x").with_attrs(ATTR_ITALIC)).italic);
        assert!(style(&Cell::new("x").with_attrs(ATTR_UNDERLINE)).underline);
        assert!(style(&Cell::new("x").with_attrs(ATTR_STRIKETHROUGH)).strikethrough);

        // Every bit in the protocol's list has to reach a field above, or a
        // style would be silently dropped on the way to the GPU.
        for &bit in inkstone_rpc::ATTRS {
            let styled = style(&Cell::new("x").with_attrs(bit));
            let distinguishes = styled.bold
                || styled.italic
                || styled.underline
                || styled.strikethrough
                || styled.fg != plain.fg
                || styled.bg != plain.bg;
            assert!(distinguishes, "attribute bit {bit} does nothing");
        }
    }
}
