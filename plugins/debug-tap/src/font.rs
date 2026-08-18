//! A 5x7 pixel font, just big enough to label the capture plot.
//!
//! The image exists to be read -- by a person, and by a vision model that has no
//! access to the numbers behind it. Gridlines without values are decoration, so
//! the renderer bakes the tick values into the pixels. Pulling in a font crate
//! (and a font file, and a rasteriser, and hinting) for eleven axis labels would
//! be absurd, so the glyphs are here, written as rows of `#` and `.` that can be
//! proof-read by looking at them.
//!
//! Uppercase only. [`glyph`] folds lowercase input, and anything unknown renders
//! as a filled box so a missing glyph is obvious rather than invisible.

/// Glyph width in pixels.
pub const GLYPH_WIDTH: usize = 5;
/// Glyph height in pixels.
pub const GLYPH_HEIGHT: usize = 7;
/// Blank columns between glyphs.
pub const GLYPH_SPACING: usize = 1;

/// Advance of one character, in unscaled pixels.
pub const GLYPH_ADVANCE: usize = GLYPH_WIDTH + GLYPH_SPACING;

type Rows = [&'static str; GLYPH_HEIGHT];

const UNKNOWN: Rows = [
    "#####", "#####", "#####", "#####", "#####", "#####", "#####",
];

const SPACE: Rows = [
    ".....", ".....", ".....", ".....", ".....", ".....", ".....",
];

/// `(character, 7 rows of 5 columns)`. Kept sorted by character for readability.
const GLYPHS: &[(char, Rows)] = &[
    (
        '!',
        [
            "..#..", "..#..", "..#..", "..#..", "..#..", ".....", "..#..",
        ],
    ),
    (
        '%',
        [
            "##..#", "##.#.", "..#..", ".#...", "#..##", "....#", ".....",
        ],
    ),
    (
        '(',
        [
            "..#..", ".#...", "#....", "#....", "#....", ".#...", "..#..",
        ],
    ),
    (
        ')',
        [
            "..#..", "...#.", "....#", "....#", "....#", "...#.", "..#..",
        ],
    ),
    (
        '+',
        [
            ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....",
        ],
    ),
    (
        ',',
        [
            ".....", ".....", ".....", ".....", ".##..", ".##..", ".#...",
        ],
    ),
    (
        '-',
        [
            ".....", ".....", ".....", "#####", ".....", ".....", ".....",
        ],
    ),
    (
        '.',
        [
            ".....", ".....", ".....", ".....", ".....", ".##..", ".##..",
        ],
    ),
    (
        '/',
        [
            "....#", "....#", "...#.", "..#..", ".#...", "#....", "#....",
        ],
    ),
    (
        '0',
        [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
    ),
    (
        '1',
        [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
    ),
    (
        '2',
        [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
    ),
    (
        '3',
        [
            "#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###.",
        ],
    ),
    (
        '4',
        [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
    ),
    (
        '5',
        [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
    ),
    (
        '6',
        [
            "..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###.",
        ],
    ),
    (
        '7',
        [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
    ),
    (
        '8',
        [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
    ),
    (
        '9',
        [
            ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..",
        ],
    ),
    (
        ':',
        [
            ".....", ".##..", ".##..", ".....", ".##..", ".##..", ".....",
        ],
    ),
    (
        '=',
        [
            ".....", ".....", "#####", ".....", "#####", ".....", ".....",
        ],
    ),
    (
        '?',
        [
            ".###.", "#...#", "....#", "...#.", "..#..", ".....", "..#..",
        ],
    ),
    (
        'A',
        [
            ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'B',
        [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
    ),
    (
        'C',
        [
            ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.",
        ],
    ),
    (
        'D',
        [
            "###..", "#..#.", "#...#", "#...#", "#...#", "#..#.", "###..",
        ],
    ),
    (
        'E',
        [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
    ),
    (
        'F',
        [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
    ),
    (
        'G',
        [
            ".###.", "#...#", "#....", "#..##", "#...#", "#...#", ".####",
        ],
    ),
    (
        'H',
        [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'I',
        [
            ".###.", "..#..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
    ),
    (
        'J',
        [
            "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##..",
        ],
    ),
    (
        'K',
        [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
    ),
    (
        'L',
        [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
    ),
    (
        'M',
        [
            "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'N',
        [
            "#...#", "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#",
        ],
    ),
    (
        'O',
        [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'P',
        [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
    ),
    (
        'Q',
        [
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
        ],
    ),
    (
        'R',
        [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
    ),
    (
        'S',
        [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
    ),
    (
        'T',
        [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
    ),
    (
        'U',
        [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'V',
        [
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
    ),
    (
        'W',
        [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#",
        ],
    ),
    (
        'X',
        [
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ],
    ),
    (
        'Y',
        [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
    ),
    (
        'Z',
        [
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
    ),
    (
        '_',
        [
            ".....", ".....", ".....", ".....", ".....", ".....", "#####",
        ],
    ),
];

/// Rows for `character`, folding case. Unknown characters render as a box.
pub fn glyph(character: char) -> &'static Rows {
    if character == ' ' {
        return &SPACE;
    }
    let upper = character.to_ascii_uppercase();
    GLYPHS
        .iter()
        .find(|(candidate, _)| *candidate == upper)
        .map(|(_, rows)| rows)
        .unwrap_or(&UNKNOWN)
}

/// Whether a glyph is defined for `character` (space counts).
pub fn has_glyph(character: char) -> bool {
    character == ' '
        || GLYPHS
            .iter()
            .any(|(candidate, _)| *candidate == character.to_ascii_uppercase())
}

/// Width of `text` in unscaled pixels, at scale 1.
pub fn text_width(text: &str) -> usize {
    let count = text.chars().count();
    if count == 0 {
        0
    } else {
        count * GLYPH_ADVANCE - GLYPH_SPACING
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_is_seven_rows_of_five_columns() {
        for (character, rows) in GLYPHS {
            assert_eq!(rows.len(), GLYPH_HEIGHT, "{character}");
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(
                    row.chars().count(),
                    GLYPH_WIDTH,
                    "glyph '{character}' row {index} is '{row}'"
                );
                assert!(
                    row.chars().all(|c| c == '#' || c == '.'),
                    "glyph '{character}' row {index} has stray characters: '{row}'"
                );
            }
        }
        assert_eq!(UNKNOWN.len(), GLYPH_HEIGHT);
        assert_eq!(SPACE.len(), GLYPH_HEIGHT);
    }

    #[test]
    fn the_table_has_no_duplicates_and_stays_sorted() {
        let mut previous: Option<char> = None;
        for (character, _) in GLYPHS {
            if let Some(previous) = previous {
                assert!(
                    previous < *character,
                    "'{previous}' and '{character}' are out of order or duplicated"
                );
            }
            previous = Some(*character);
        }
    }

    #[test]
    fn every_character_the_renderer_uses_has_a_glyph() {
        // The axis labels, the panel titles and the legend, spelled out. If the
        // renderer grows a new word, this is where it is noticed.
        for text in [
            "WAVEFORM L=BLUE R=ORANGE",
            "SPECTRUM DBFS VS HZ (LOG)",
            "0 -6 -12 -24 -48 DB",
            "20 50 100 200 500 1K 2K 5K 10K 20K HZ",
            "-20 -40 -60 -80 -100",
            "440HZ PEAK",
            "NO SPECTRUM: CAPTURE TOO SHORT",
            "SILENCE",
            "TIME 0.000S 1.234S",
            "NAN/INF",
        ] {
            for character in text.chars() {
                assert!(has_glyph(character), "no glyph for '{character}' in {text}");
            }
        }
    }

    #[test]
    fn lowercase_folds_to_uppercase() {
        assert_eq!(glyph('a'), glyph('A'));
        assert_eq!(glyph('z'), glyph('Z'));
    }

    #[test]
    fn an_unknown_character_is_a_visible_box_not_a_hole() {
        assert_eq!(glyph('\u{263A}'), &UNKNOWN);
        assert!(!has_glyph('\u{263A}'));
    }

    #[test]
    fn text_width_accounts_for_the_gaps_between_glyphs() {
        assert_eq!(text_width(""), 0);
        assert_eq!(text_width("A"), GLYPH_WIDTH);
        assert_eq!(text_width("AB"), GLYPH_WIDTH * 2 + GLYPH_SPACING);
    }
}
