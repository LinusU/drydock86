//! S3TC (DXT1/3/5) block decompression to RGBA8.

use crate::surface::Fmt;

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [((r << 3) | (r >> 2)) as u8, ((g << 2) | (g >> 4)) as u8, ((b << 3) | (b >> 2)) as u8]
}

fn color_block(b: &[u8], out: &mut [[u8; 4]; 16], dxt1: bool) {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || !dxt1 {
        for k in 0..3 {
            pal[2][k] = ((2 * p0[k] as u32 + p1[k] as u32) / 3) as u8;
            pal[3][k] = ((p0[k] as u32 + 2 * p1[k] as u32) / 3) as u8;
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for k in 0..3 {
            pal[2][k] = ((p0[k] as u32 + p1[k] as u32) / 2) as u8;
        }
        pal[2][3] = 255;
        pal[3] = [0, 0, 0, 0];
    }
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    for (i, px) in out.iter_mut().enumerate() {
        *px = pal[((bits >> (2 * i)) & 3) as usize];
    }
}

pub fn decode(fmt: Fmt, w: u32, h: u32, data: &[u8]) -> Vec<u8> {
    let bw = w.div_ceil(4);
    let bh = h.div_ceil(4);
    let bsize = if fmt == Fmt::Dxt1 { 8 } else { 16 };
    let mut out = vec![0u8; (w * h * 4) as usize];
    for by in 0..bh {
        for bx in 0..bw {
            let o = ((by * bw + bx) * bsize) as usize;
            if o + bsize as usize > data.len() {
                continue;
            }
            let blk = &data[o..o + bsize as usize];
            let mut px = [[0u8; 4]; 16];
            match fmt {
                Fmt::Dxt1 => color_block(blk, &mut px, true),
                Fmt::Dxt3 => {
                    color_block(&blk[8..], &mut px, false);
                    for (i, p) in px.iter_mut().enumerate() {
                        let a = (blk[i / 2] >> ((i % 2) * 4)) & 15;
                        p[3] = a * 17;
                    }
                }
                _ => {
                    color_block(&blk[8..], &mut px, false);
                    let (a0, a1) = (blk[0] as u32, blk[1] as u32);
                    let mut alpha = [0u8; 8];
                    alpha[0] = a0 as u8;
                    alpha[1] = a1 as u8;
                    for k in 1..7u32 {
                        alpha[k as usize + 1] = if a0 > a1 {
                            (((7 - k) * a0 + k * a1) / 7) as u8
                        } else if k < 5 {
                            (((5 - k) * a0 + k * a1) / 5) as u8
                        } else if k == 5 {
                            0
                        } else {
                            255
                        };
                    }
                    let bits = u64::from_le_bytes([blk[2], blk[3], blk[4], blk[5], blk[6], blk[7], 0, 0]);
                    for (i, p) in px.iter_mut().enumerate() {
                        p[3] = alpha[((bits >> (3 * i)) & 7) as usize];
                    }
                }
            }
            for (i, p) in px.iter().enumerate() {
                let x = bx * 4 + (i as u32 % 4);
                let y = by * 4 + (i as u32 / 4);
                if x < w && y < h {
                    let d = ((y * w + x) * 4) as usize;
                    out[d..d + 4].copy_from_slice(p);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dxt1_solid_block() {
        // c0 = pure red 0xf800, c1 = 0, all indices 0.
        let blk = [0x00, 0xf8, 0, 0, 0, 0, 0, 0];
        let out = decode(Fmt::Dxt1, 4, 4, &blk);
        assert_eq!(&out[0..4], &[255, 0, 0, 255]);
        assert_eq!(&out[60..64], &[255, 0, 0, 255]);
    }
}
