//! Fast compressed Kitty graphics protocol (RGB24 + zlib) with modal occlusion splitting.
use flate2::{write::ZlibEncoder, Compression};
use image::RgbaImage;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Color,
};
use std::io::Write;

pub const ID_A: u32 = 0x00A6_0F01;
pub const ID_B: u32 = 0x00A6_0F02;

/// Encode an opaque RGBA image as compressed 24-bit RGB (`f=24,o=z`) Kitty virtual placement (`U=1`).
pub fn encode_rgb_zlib(img: &RgbaImage, id: u32, area: Rect, is_tmux: bool, rgb_scratch: &mut Vec<u8>) -> Vec<u8> {
    let (w, h) = (img.width(), img.height());
    let (cols, rows) = (area.width.max(1), area.height.max(1));

    rgb_scratch.clear();
    rgb_scratch.reserve((w as usize) * (h as usize) * 3);
    for px in img.as_raw().chunks_exact(4) {
        rgb_scratch.push(px[0]);
        rgb_scratch.push(px[1]);
        rgb_scratch.push(px[2]);
    }

    let mut enc = ZlibEncoder::new(Vec::with_capacity(rgb_scratch.len() / 4), Compression::fast());
    let _ = enc.write_all(rgb_scratch);
    let compressed = enc.finish().unwrap_or_default();

    const CHARS_PER_CHUNK: usize = 4096;
    const CHUNK_BYTES: usize = (CHARS_PER_CHUNK / 4) * 3;
    let chunks: Vec<&[u8]> = if compressed.is_empty() {
        vec![&[][..]]
    } else {
        compressed.chunks(CHUNK_BYTES).collect()
    };
    let n = chunks.len();

    let (start, esc, end) = if is_tmux {
        ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "")
    };

    let mut out = Vec::with_capacity(compressed.len() * 4 / 3 + n * 64 + 32);
    out.extend_from_slice(start.as_bytes());
    let mut b64_buf = Vec::with_capacity(CHARS_PER_CHUNK + 4);
    for (i, chunk) in chunks.iter().enumerate() {
        b64_buf.resize(base64_simd::STANDARD.encoded_length(chunk.len()), 0);
        let encoded = base64_simd::STANDARD.encode_as_str(chunk, base64_simd::Out::from_slice(&mut b64_buf));
        let more = u8::from(i + 1 < n);
        if i == 0 {
            let _ = write!(
                out,
                "{esc}_Gq=2,i={id},a=T,U=1,f=24,o=z,t=d,s={w},v={h},c={cols},r={rows},m={more};{encoded}{esc}\\"
            );
        } else {
            let _ = write!(out, "{esc}_Gq=2,m={more};{encoded}{esc}\\");
        }
    }
    out.extend_from_slice(end.as_bytes());
    out
}

/// Delete both double-buffered Kitty image placements so Ghostty frees GPU textures on exit.
pub fn delete_seqs(is_tmux: bool) -> Vec<u8> {
    let (start, esc, end) = if is_tmux {
        ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "")
    };
    let mut out = Vec::new();
    out.extend_from_slice(start.as_bytes());
    for id in [ID_A, ID_B] {
        let _ = write!(out, "{esc}_Ga=d,d=i,i={id},q=2;{esc}\\");
    }
    out.extend_from_slice(end.as_bytes());
    out
}

/// Write self-contained `\u{10EEEE}` Unicode placeholder cells across `area`, skipping `occluded`
/// so centered modals float cleanly above the Kitty image layer without bleed-through.
pub fn render_placeholders(area: Rect, buf: &mut Buffer, id: u32, occluded: Option<Rect>) {
    let [id_extra, id_r, id_g, id_b] = id.to_be_bytes();
    let fg = Color::Rgb(id_r, id_g, id_b);
    let extra_d = diacritic(u16::from(id_extra));
    let mut sym = [0u8; 16];

    for y in 0..area.height {
        let abs_y = area.top() + y;
        let row_d = diacritic(y);
        let occ_span = occluded.and_then(|o| {
            if abs_y >= o.top() && abs_y < o.bottom() {
                let s = o.left().saturating_sub(area.left()).min(area.width);
                let e = o.right().saturating_sub(area.left()).min(area.width);
                (s < e).then_some((s, e))
            } else {
                None
            }
        });
        for x in 0..area.width {
            if let Some((os, oe)) = occ_span {
                if x >= os && x < oe {
                    continue;
                }
            }
            let col_d = diacritic(x);
            let s = encode_cell_placeholder(&mut sym, row_d, col_d, extra_d);
            if let Some(cell) = buf.cell_mut((area.left() + x, abs_y)) {
                cell.set_symbol(s);
                cell.set_fg(fg);
                cell.set_bg(Color::Reset);
            }
        }
    }
}

#[inline]
fn encode_cell_placeholder(buf: &mut [u8; 16], row_d: char, col_d: char, extra_d: char) -> &str {
    let mut len = 0;
    for ch in ['\u{10EEEE}', row_d, col_d, extra_d] {
        len += ch.encode_utf8(&mut buf[len..]).len();
    }
    // SAFETY: all slices written by char::encode_utf8 are valid UTF-8.
    unsafe { std::str::from_utf8_unchecked(&buf[..len]) }
}

/// From https://sw.kovidgoyal.net/kitty/_downloads/1792bad15b12979994cd6ecc54c967a6/rowcolumn-diacritics.txt
static DIACRITICS: [char; 297] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}', '\u{346}',
    '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}', '\u{35B}', '\u{363}',
    '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}', '\u{36A}', '\u{36B}', '\u{36C}',
    '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}', '\u{485}', '\u{486}', '\u{487}', '\u{592}',
    '\u{593}', '\u{594}', '\u{595}', '\u{597}', '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}',
    '\u{59F}', '\u{5A0}', '\u{5A1}', '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}',
    '\u{610}', '\u{611}', '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}',
    '\u{658}', '\u{659}', '\u{65A}', '\u{65B}', '\u{65D}', '\u{65E}', '\u{6D6}', '\u{6D7}', '\u{6D8}',
    '\u{6D9}', '\u{6DA}', '\u{6DB}', '\u{6DC}', '\u{6DF}', '\u{6E0}', '\u{6E1}', '\u{6E2}', '\u{6E4}',
    '\u{6E7}', '\u{6E8}', '\u{6EB}', '\u{6EC}', '\u{730}', '\u{732}', '\u{733}', '\u{735}', '\u{736}',
    '\u{73A}', '\u{73D}', '\u{73F}', '\u{740}', '\u{741}', '\u{743}', '\u{745}', '\u{747}', '\u{749}',
    '\u{74A}', '\u{7EB}', '\u{7EC}', '\u{7ED}', '\u{7EE}', '\u{7EF}', '\u{7F0}', '\u{7F1}', '\u{7F3}',
    '\u{816}', '\u{817}', '\u{818}', '\u{819}', '\u{81B}', '\u{81C}', '\u{81D}', '\u{81E}', '\u{81F}',
    '\u{820}', '\u{821}', '\u{822}', '\u{823}', '\u{825}', '\u{826}', '\u{827}', '\u{829}', '\u{82A}',
    '\u{82B}', '\u{82C}', '\u{82D}', '\u{951}', '\u{953}', '\u{954}', '\u{F82}', '\u{F83}', '\u{F86}',
    '\u{F87}', '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}',
    '\u{1A76}', '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}',
    '\u{1B6D}', '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}',
    '\u{1CD1}', '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}',
    '\u{1DC4}', '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}',
    '\u{1DD1}', '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}',
    '\u{1DD9}', '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}',
    '\u{1DE1}', '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}',
    '\u{20D1}', '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}',
    '\u{20E7}', '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}',
    '\u{2DE2}', '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}',
    '\u{2DEA}', '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}',
    '\u{2DF2}', '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}',
    '\u{2DFA}', '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}',
    '\u{A67D}', '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}',
    '\u{A8E5}', '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}', '\u{A8EB}', '\u{A8EC}',
    '\u{A8ED}', '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}', '\u{AAB3}',
    '\u{AAB7}', '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}',
    '\u{FE23}', '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}',
    '\u{1D187}', '\u{1D188}', '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}',
    '\u{1D243}', '\u{1D244}',
];

#[inline]
fn diacritic(i: u16) -> char {
    *DIACRITICS.get(usize::from(i)).unwrap_or(&DIACRITICS[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn zlib_rgb24_compresses_flat_pixel_art() {
        let img = RgbaImage::from_pixel(640, 360, Rgba([28, 34, 48, 255]));
        let mut scratch = Vec::new();
        let bytes = encode_rgb_zlib(&img, ID_A, Rect::new(0, 0, 80, 24), false, &mut scratch);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("f=24,o=z,t=d"));
        assert!(text.contains("U=1"));
        assert!(text.contains("c=80,r=24"));
        // 640x360 raw RGBA is ~921 KB; compressed base64 should be tiny (< 10 KB).
        assert!(text.len() < 10_000, "unexpected size: {}", text.len());
    }

    #[test]
    fn tmux_passthrough_escapes_apc() {
        let img = RgbaImage::from_pixel(16, 16, Rgba([10, 20, 30, 255]));
        let mut scratch = Vec::new();
        let text = String::from_utf8(encode_rgb_zlib(&img, ID_B, Rect::new(0, 0, 2, 1), true, &mut scratch)).unwrap();
        assert!(text.starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(text.ends_with("\x1b\x1b\\\x1b\\"));
    }

    #[test]
    fn occlusion_splitting_preserves_modal_cells_and_column_diacritics() {
        let area = Rect::new(0, 0, 10, 5);
        let modal = Rect::new(3, 1, 4, 3);
        let mut buf = Buffer::empty(area);
        render_placeholders(area, &mut buf, ID_A, Some(modal));

        // Inside modal: cells remain empty spaces (not overwritten by placeholders).
        assert_eq!(buf[(4, 2)].symbol(), " ");
        // Left of modal on row 1: col 2 has col diacritic(2) = \u{030E}.
        assert!(buf[(2, 1)].symbol().starts_with('\u{10EEEE}'));
        assert!(buf[(2, 1)].symbol().contains(diacritic(2)));
        // Right of modal on row 1: col 7 has row diacritic(1) and col diacritic(7).
        assert!(buf[(7, 1)].symbol().contains(diacritic(1)));
        assert!(buf[(7, 1)].symbol().contains(diacritic(7)));
        assert_eq!(buf[(7, 1)].fg, Color::Rgb(0xA6, 0x0F, 0x01));
    }
}
