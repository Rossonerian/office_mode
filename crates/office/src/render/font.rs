//! A tiny built-in 3×5 bitmap font (uppercase; lowercase maps to uppercase). 4 px advance.
use super::canvas::{fill, put, Col};
use image::RgbaImage;

pub const GLYPH_W: i32 = 3;
pub const GLYPH_H: i32 = 5;
pub const ADVANCE: i32 = 4;

/// Each glyph: 5 rows of 3 bits (MSB = left pixel), packed top row first.
const fn g(r: [u8; 5]) -> u16 {
    ((r[0] as u16) << 12) | ((r[1] as u16) << 9) | ((r[2] as u16) << 6) | ((r[3] as u16) << 3) | r[4] as u16
}

pub fn glyph(ch: char) -> u16 {
    let c = ch.to_ascii_uppercase();
    match c {
        'A' => g([0b010, 0b101, 0b111, 0b101, 0b101]),
        'B' => g([0b110, 0b101, 0b110, 0b101, 0b110]),
        'C' => g([0b011, 0b100, 0b100, 0b100, 0b011]),
        'D' => g([0b110, 0b101, 0b101, 0b101, 0b110]),
        'E' => g([0b111, 0b100, 0b110, 0b100, 0b111]),
        'F' => g([0b111, 0b100, 0b110, 0b100, 0b100]),
        'G' => g([0b011, 0b100, 0b101, 0b101, 0b011]),
        'H' => g([0b101, 0b101, 0b111, 0b101, 0b101]),
        'I' => g([0b111, 0b010, 0b010, 0b010, 0b111]),
        'J' => g([0b001, 0b001, 0b001, 0b101, 0b010]),
        'K' => g([0b101, 0b101, 0b110, 0b101, 0b101]),
        'L' => g([0b100, 0b100, 0b100, 0b100, 0b111]),
        'M' => g([0b101, 0b111, 0b111, 0b101, 0b101]),
        'N' => g([0b110, 0b101, 0b101, 0b101, 0b101]),
        'O' => g([0b010, 0b101, 0b101, 0b101, 0b010]),
        'P' => g([0b110, 0b101, 0b110, 0b100, 0b100]),
        'Q' => g([0b010, 0b101, 0b101, 0b110, 0b011]),
        'R' => g([0b110, 0b101, 0b110, 0b101, 0b101]),
        'S' => g([0b011, 0b100, 0b010, 0b001, 0b110]),
        'T' => g([0b111, 0b010, 0b010, 0b010, 0b010]),
        'U' => g([0b101, 0b101, 0b101, 0b101, 0b111]),
        'V' => g([0b101, 0b101, 0b101, 0b101, 0b010]),
        'W' => g([0b101, 0b101, 0b111, 0b111, 0b101]),
        'X' => g([0b101, 0b101, 0b010, 0b101, 0b101]),
        'Y' => g([0b101, 0b101, 0b010, 0b010, 0b010]),
        'Z' => g([0b111, 0b001, 0b010, 0b100, 0b111]),
        '0' => g([0b111, 0b101, 0b101, 0b101, 0b111]),
        '1' => g([0b010, 0b110, 0b010, 0b010, 0b111]),
        '2' => g([0b110, 0b001, 0b010, 0b100, 0b111]),
        '3' => g([0b110, 0b001, 0b010, 0b001, 0b110]),
        '4' => g([0b101, 0b101, 0b111, 0b001, 0b001]),
        '5' => g([0b111, 0b100, 0b110, 0b001, 0b110]),
        '6' => g([0b011, 0b100, 0b111, 0b101, 0b111]),
        '7' => g([0b111, 0b001, 0b010, 0b010, 0b010]),
        '8' => g([0b111, 0b101, 0b111, 0b101, 0b111]),
        '9' => g([0b111, 0b101, 0b111, 0b001, 0b110]),
        ' ' => 0,
        '.' => g([0, 0, 0, 0, 0b010]),
        ',' => g([0, 0, 0, 0b010, 0b100]),
        ':' => g([0, 0b010, 0, 0b010, 0]),
        ';' => g([0, 0b010, 0, 0b010, 0b100]),
        '-' => g([0, 0, 0b111, 0, 0]),
        '_' => g([0, 0, 0, 0, 0b111]),
        '/' => g([0b001, 0b001, 0b010, 0b100, 0b100]),
        '\\' => g([0b100, 0b100, 0b010, 0b001, 0b001]),
        '(' => g([0b010, 0b100, 0b100, 0b100, 0b010]),
        ')' => g([0b010, 0b001, 0b001, 0b001, 0b010]),
        '[' => g([0b110, 0b100, 0b100, 0b100, 0b110]),
        ']' => g([0b011, 0b001, 0b001, 0b001, 0b011]),
        '{' => g([0b011, 0b010, 0b110, 0b010, 0b011]),
        '}' => g([0b110, 0b010, 0b011, 0b010, 0b110]),
        '!' => g([0b010, 0b010, 0b010, 0, 0b010]),
        '?' => g([0b110, 0b001, 0b010, 0, 0b010]),
        '\'' => g([0b010, 0b010, 0, 0, 0]),
        '"' => g([0b101, 0b101, 0, 0, 0]),
        '`' => g([0b100, 0b010, 0, 0, 0]),
        '^' => g([0b010, 0b101, 0, 0, 0]),
        '+' => g([0, 0b010, 0b111, 0b010, 0]),
        '=' => g([0, 0b111, 0, 0b111, 0]),
        '*' => g([0, 0b101, 0b010, 0b101, 0]),
        '#' => g([0b101, 0b111, 0b101, 0b111, 0b101]),
        '%' => g([0b101, 0b001, 0b010, 0b100, 0b101]),
        '$' => g([0b011, 0b110, 0b010, 0b011, 0b110]),
        '@' => g([0b010, 0b101, 0b111, 0b100, 0b011]),
        '&' => g([0b010, 0b101, 0b010, 0b101, 0b011]),
        '<' => g([0b001, 0b010, 0b100, 0b010, 0b001]),
        '>' => g([0b100, 0b010, 0b001, 0b010, 0b100]),
        '|' => g([0b010, 0b010, 0b010, 0b010, 0b010]),
        '~' => g([0, 0b011, 0b110, 0, 0]),
        '·' | '•' => g([0, 0, 0b010, 0, 0]),
        '…' => g([0, 0, 0, 0, 0b101]),
        '→' => g([0, 0b010, 0b111, 0b010, 0]),
        '–' | '—' => g([0, 0, 0b111, 0, 0]),
        '‘' | '’' => g([0b010, 0b010, 0, 0, 0]),
        '“' | '”' => g([0b101, 0b101, 0, 0, 0]),
        '└' | '├' => g([0b100, 0b100, 0b100, 0b100, 0b111]),
        '✓' => g([0, 0b001, 0b010, 0b101, 0b010]),
        '✕' => g([0b101, 0b101, 0b010, 0b101, 0b101]),
        _ => g([0b110, 0b001, 0b010, 0, 0b010]),
    }
}

pub fn text_width(s: &str) -> i32 {
    let n = s.chars().count() as i32;
    if n == 0 { 0 } else { n * ADVANCE - 1 }
}

/// Truncate to at most `max_px` wide, ending with '…' when cut.
pub fn truncate(s: &str, max_px: i32) -> String {
    let max_chars = ((max_px + 1) / ADVANCE).max(0) as usize;
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

pub fn draw_text(img: &mut RgbaImage, x: i32, y: i32, s: &str, c: Col) {
    let mut cx = x;
    for ch in s.chars() {
        let bits = glyph(ch);
        for row in 0..GLYPH_H {
            for col in 0..GLYPH_W {
                let bit = 14 - (row * 3 + col);
                if bits >> bit & 1 == 1 {
                    put(img, cx + col, y + row, c);
                }
            }
        }
        cx += ADVANCE;
    }
}

/// Text on a solid backing box (1 px padding), centred on `cx`.
pub fn label(img: &mut RgbaImage, cx: i32, y: i32, s: &str, fg: Col, bg: Col) {
    let w = text_width(s);
    let x = cx - w / 2;
    fill(img, x - 1, y - 1, w + 2, GLYPH_H + 2, bg);
    draw_text(img, x, y, s, fg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_truncation() {
        assert_eq!(text_width(""), 0);
        assert_eq!(text_width("A"), 3);
        assert_eq!(text_width("ABC"), 11);
        assert_eq!(truncate("hello", 100), "hello");
        let t = truncate("hello world", 19);
        assert_eq!(t.chars().count(), 5);
        assert!(t.ends_with('…'));
        assert!(text_width(&t) <= 19);
    }

    #[test]
    fn glyphs_distinct_and_lowercase_maps() {
        assert_eq!(glyph('a'), glyph('A'));
        let alnum: Vec<u16> = ('A'..='Z').chain('0'..='9').map(glyph).collect();
        for (i, a) in alnum.iter().enumerate() {
            assert_ne!(*a, 0, "glyph {i} empty");
            for b in &alnum[i + 1..] {
                // O and 0 differ, etc.: every alnum glyph is unique
                assert_ne!(a, b);
            }
        }
        assert_eq!(glyph(' '), 0);
    }

    #[test]
    fn draws_pixels() {
        let mut img = RgbaImage::new(20, 8);
        draw_text(&mut img, 0, 0, "I", [255, 0, 0, 255]);
        // 'I' top row is 111
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(0, 1).0[3], 0);
    }
}
