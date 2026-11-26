//! Tiny raster helpers over `image::RgbaImage` with signed, clipped coordinates.
use image::RgbaImage;

pub type Col = [u8; 4];

pub const fn rgb(hex: u32) -> Col {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255]
}

pub fn parse_hex(s: &str) -> Option<Col> {
    let s = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    match s.len() {
        6 => Some(rgb(v)),
        8 => Some([(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]),
        _ => None,
    }
}

/// Multiply brightness by `f` (0.0..2.0), keeping alpha.
pub fn shade(c: Col, f: f32) -> Col {
    let m = |v: u8| (v as f32 * f).round().clamp(0.0, 255.0) as u8;
    [m(c[0]), m(c[1]), m(c[2]), c[3]]
}

pub fn with_alpha(c: Col, a: u8) -> Col {
    [c[0], c[1], c[2], a]
}

#[inline]
fn blend(dst: &mut [u8], c: Col) {
    match c[3] {
        0 => {}
        255 => dst[..4].copy_from_slice(&c),
        a => {
            let a = a as u32;
            let ia = 255 - a;
            for i in 0..3 {
                dst[i] = ((c[i] as u32 * a + dst[i] as u32 * ia) / 255) as u8;
            }
            dst[3] = dst[3].max(c[3]);
        }
    }
}

#[inline]
pub fn put(img: &mut RgbaImage, x: i32, y: i32, c: Col) {
    let (w, h) = img.dimensions();
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let i = (y as usize * w as usize + x as usize) * 4;
    blend(&mut img.as_mut()[i..i + 4], c);
}

/// Additive light (for glows).
pub fn add(img: &mut RgbaImage, x: i32, y: i32, c: Col, k: f32) {
    let (w, h) = img.dimensions();
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let i = (y as usize * w as usize + x as usize) * 4;
    let p = &mut img.as_mut()[i..i + 4];
    for j in 0..3 {
        p[j] = (p[j] as f32 + c[j] as f32 * k).min(255.0) as u8;
    }
}

pub fn fill(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Col) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + w).min(iw), (y + h).min(ih));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let buf = img.as_mut();
    for yy in y0..y1 {
        let row = (yy * iw) as usize * 4;
        for xx in x0..x1 {
            let i = row + xx as usize * 4;
            blend(&mut buf[i..i + 4], c);
        }
    }
}

pub fn outline(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Col) {
    fill(img, x, y, w, 1, c);
    fill(img, x, y + h - 1, w, 1, c);
    fill(img, x, y, 1, h, c);
    fill(img, x + w - 1, y, 1, h, c);
}

/// Alpha-blit `src` at (x, y).
pub fn blit(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32) {
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let sx0 = (-x).max(0);
    let sy0 = (-y).max(0);
    let sx1 = sw.min(dw - x);
    let sy1 = sh.min(dh - y);
    if sx0 >= sx1 || sy0 >= sy1 {
        return;
    }
    let s = src.as_raw();
    let d = dst.as_mut();
    for sy in sy0..sy1 {
        let srow = (sy * sw) as usize * 4;
        let drow = ((sy + y) * dw) as usize * 4;
        for sx in sx0..sx1 {
            let si = srow + sx as usize * 4;
            let a = s[si + 3];
            if a == 0 {
                continue;
            }
            let di = drow + (sx + x) as usize * 4;
            blend(&mut d[di..di + 4], [s[si], s[si + 1], s[si + 2], a]);
        }
    }
}

/// Copy a sub-rectangle out of a sheet (clipped to the sheet).
pub fn crop(src: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> RgbaImage {
    let mut out = RgbaImage::new(w, h);
    for yy in 0..h {
        for xx in 0..w {
            if x + xx < src.width() && y + yy < src.height() {
                out.put_pixel(xx, yy, *src.get_pixel(x + xx, y + yy));
            }
        }
    }
    out
}

/// Map every non-transparent pixel through `f`.
pub fn map_pixels(img: &mut RgbaImage, f: &dyn Fn(Col) -> Col) {
    for p in img.as_mut().as_chunks_mut::<4>().0 {
        if p[3] == 0 {
            continue;
        }
        let c = f([p[0], p[1], p[2], p[3]]);
        p.copy_from_slice(&c);
    }
}

/// Nearest-neighbour scale of `src` into `dst` at (ox, oy) with factor `s` (may be < 1).
pub fn scale_into(dst: &mut RgbaImage, src: &RgbaImage, ox: u32, oy: u32, s: f32) {
    let mut xmap = Vec::new();
    scale_into_with_xmap(dst, src, ox, oy, s, &mut xmap);
}

/// Nearest-neighbour scale reusing a caller-owned `xmap` buffer across frames.
pub fn scale_into_with_xmap(dst: &mut RgbaImage, src: &RgbaImage, ox: u32, oy: u32, s: f32, xmap: &mut Vec<usize>) {
    let (dw, dh) = (dst.width(), dst.height());
    let (sw, sh) = (src.width(), src.height());
    let ow = ((sw as f32 * s).floor() as u32).min(dw.saturating_sub(ox));
    let oh = ((sh as f32 * s).floor() as u32).min(dh.saturating_sub(oy));
    if ow == 0 || oh == 0 {
        return;
    }
    xmap.clear();
    xmap.extend((0..ow).map(|x| ((x as f32 / s) as u32).min(sw - 1) as usize * 4));
    let sraw = src.as_raw();
    let draw = dst.as_mut();
    let mut last_sy = u32::MAX;
    let mut last_row = 0usize;
    for y in 0..oh {
        let sy = ((y as f32 / s) as u32).min(sh - 1);
        let drow = ((y + oy) * dw + ox) as usize * 4;
        if sy == last_sy {
            // duplicate the previous output row
            let (a, b) = draw.split_at_mut(drow);
            b[..ow as usize * 4].copy_from_slice(&a[last_row..last_row + ow as usize * 4]);
            continue;
        }
        let srow = (sy * sw) as usize * 4;
        let out = &mut draw[drow..drow + ow as usize * 4];
        if s == 1.0 {
            out.copy_from_slice(&sraw[srow..srow + ow as usize * 4]);
        } else {
            let row = &sraw[srow..srow + sw as usize * 4];
            for (o, &sx) in out.as_chunks_mut::<4>().0.iter_mut().zip(xmap.iter()) {
                o.copy_from_slice(&row[sx..sx + 4]);
            }
        }
        last_sy = sy;
        last_row = drow;
    }
}

