// Inverse transforms, motion compensation and wavelet recomposition for the Indeo 5
// decoder. Ported from FFmpeg n7.1 libavcodec/ivi_dsp.c.
//
// Copyright (c) 2009-2011 Maxim Poliakovski
//
// FFmpeg is free software; you can redistribute it and/or modify it under the terms
// of the GNU Lesser General Public License as published by the Free Software
// Foundation; either version 2.1 of the License, or (at your option) any later
// version. As permitted by section 3 of that licence, this derived file is
// distributed under the terms of the GNU General Public License version 2, as part
// of Drydock.
//
// Integer behaviour follows the C code: intermediate values are `int`, results are
// stored into 16-bit band buffers by truncation, and right shifts are arithmetic.

/// An inverse transform: coefficients `input` (row-major, `size` x `size`) into the
/// band buffer at `out[off..]` with row pitch `pitch`. `flags[i]` tells whether
/// column `i` has a non-zero coefficient.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transform {
    Slant8x8,
    RowSlant8,
    ColSlant8,
    PutPixels8x8,
    Slant4x4,
}

impl Transform {
    pub fn size(self) -> usize {
        if self == Transform::Slant4x4 { 4 } else { 8 }
    }

    pub fn is_2d(self) -> bool {
        matches!(self, Transform::Slant8x8 | Transform::Slant4x4)
    }

    pub fn inverse(self, input: &[i32; 64], out: &mut [i16], off: usize, pitch: usize, flags: &[u8; 8]) {
        match self {
            Transform::Slant8x8 => inverse_slant_8x8(input, out, off, pitch, flags),
            Transform::RowSlant8 => row_slant8(input, out, off, pitch),
            Transform::ColSlant8 => col_slant8(input, out, off, pitch, flags),
            Transform::PutPixels8x8 => put_pixels_8x8(input, out, off, pitch),
            Transform::Slant4x4 => inverse_slant_4x4(input, out, off, pitch, flags),
        }
    }

    /// The transform of a block with only a DC coefficient `dc`.
    pub fn dc(self, dc: i32, out: &mut [i16], off: usize, pitch: usize, blk_size: usize) {
        match self {
            Transform::Slant8x8 | Transform::Slant4x4 => {
                let v = ((dc + 1) >> 1) as i16;
                for y in 0..blk_size {
                    out[off + y * pitch..][..blk_size].fill(v);
                }
            }
            Transform::RowSlant8 => {
                let v = ((dc + 1) >> 1) as i16;
                out[off..][..blk_size].fill(v);
                for y in 1..blk_size {
                    out[off + y * pitch..][..blk_size].fill(0);
                }
            }
            Transform::ColSlant8 => {
                let v = ((dc + 1) >> 1) as i16;
                for y in 0..blk_size {
                    let row = &mut out[off + y * pitch..][..blk_size];
                    row.fill(0);
                    row[0] = v;
                }
            }
            Transform::PutPixels8x8 => {
                for y in 0..8 {
                    out[off + y * pitch..][..8].fill(0);
                }
                out[off] = dc as i16;
            }
        }
    }
}

#[inline]
fn bfly(s1: i32, s2: i32) -> (i32, i32) {
    (s1 + s2, s1 - s2)
}

/// Reflection a,b = 1/2, 5/4.
#[inline]
fn ireflect(s1: i32, s2: i32) -> (i32, i32) {
    let o1 = ((s1 + s2 * 2 + 2) >> 2) + s1;
    let o2 = ((s1 * 2 - s2 + 2) >> 2) - s2;
    (o1, o2)
}

/// Reflection a,b = 1/2, 7/8.
#[inline]
fn slant_part4(s1: i32, s2: i32) -> (i32, i32) {
    let o1 = s2 + ((s1 * 4 - s2 + 4) >> 3);
    let o2 = s1 + ((-s1 - s2 * 4 + 4) >> 3);
    (o1, o2)
}

/// The inverse 8-point slant transform of `s` (in stream order).
#[inline]
fn inv_slant8(s: [i32; 8]) -> [i32; 8] {
    // FFmpeg's IVI_INV_SLANT8(s1, s4, s8, s5, s2, s6, s3, s7, ...).
    let [s1, s4, s8, s5, s2, s6, s3, s7] = s;
    let (t4, t5) = slant_part4(s4, s5);
    let (t1, t5) = bfly(s1, t5);
    let (t2, t6) = bfly(s2, s6);
    let (t7, t3) = bfly(s7, s3);
    let (t4, t8) = bfly(t4, s8);

    let (t1, t2) = bfly(t1, t2);
    let (t4, t3) = ireflect(t4, t3);
    let (t5, t6) = bfly(t5, t6);
    let (t8, t7) = ireflect(t8, t7);
    let (t1, t4) = bfly(t1, t4);
    let (t2, t3) = bfly(t2, t3);
    let (t5, t8) = bfly(t5, t8);
    let (t6, t7) = bfly(t6, t7);
    [t1, t2, t3, t4, t5, t6, t7, t8]
}

/// The inverse 4-point slant transform of `s` (in stream order).
#[inline]
fn inv_slant4(s: [i32; 4]) -> [i32; 4] {
    // FFmpeg's IVI_INV_SLANT4(s1, s4, s2, s3, ...).
    let [s1, s4, s2, s3] = s;
    let (t1, t2) = bfly(s1, s2);
    let (t4, t3) = ireflect(s4, s3);
    let (t1, t4) = bfly(t1, t4);
    let (t2, t3) = bfly(t2, t3);
    [t1, t2, t3, t4]
}

#[inline]
fn compensate(x: i32) -> i16 {
    ((x + 1) >> 1) as i16
}

fn inverse_slant_8x8(input: &[i32; 64], out: &mut [i16], off: usize, pitch: usize, flags: &[u8; 8]) {
    let mut tmp = [0i32; 64];
    for i in 0..8 {
        if flags[i] != 0 {
            let col = inv_slant8(std::array::from_fn(|k| input[i + 8 * k]));
            for k in 0..8 {
                tmp[i + 8 * k] = col[k];
            }
        }
    }
    for i in 0..8 {
        let src: [i32; 8] = tmp[8 * i..8 * i + 8].try_into().unwrap();
        let row = &mut out[off + i * pitch..][..8];
        if src.iter().all(|&v| v == 0) {
            row.fill(0);
        } else {
            let d = inv_slant8(src);
            for k in 0..8 {
                row[k] = compensate(d[k]);
            }
        }
    }
}

fn inverse_slant_4x4(input: &[i32; 64], out: &mut [i16], off: usize, pitch: usize, flags: &[u8; 8]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        if flags[i] != 0 {
            let col = inv_slant4(std::array::from_fn(|k| input[i + 4 * k]));
            for k in 0..4 {
                tmp[i + 4 * k] = col[k];
            }
        }
    }
    for i in 0..4 {
        let src: [i32; 4] = tmp[4 * i..4 * i + 4].try_into().unwrap();
        let row = &mut out[off + i * pitch..][..4];
        if src.iter().all(|&v| v == 0) {
            row.fill(0);
        } else {
            let d = inv_slant4(src);
            for k in 0..4 {
                row[k] = compensate(d[k]);
            }
        }
    }
}

fn row_slant8(input: &[i32; 64], out: &mut [i16], off: usize, pitch: usize) {
    for i in 0..8 {
        let src: [i32; 8] = input[8 * i..8 * i + 8].try_into().unwrap();
        let row = &mut out[off + i * pitch..][..8];
        if src.iter().all(|&v| v == 0) {
            row.fill(0);
        } else {
            let d = inv_slant8(src);
            for k in 0..8 {
                row[k] = compensate(d[k]);
            }
        }
    }
}

fn col_slant8(input: &[i32; 64], out: &mut [i16], off: usize, pitch: usize, flags: &[u8; 8]) {
    for i in 0..8 {
        if flags[i] != 0 {
            let d = inv_slant8(std::array::from_fn(|k| input[i + 8 * k]));
            for k in 0..8 {
                out[off + k * pitch + i] = compensate(d[k]);
            }
        } else {
            for k in 0..8 {
                out[off + k * pitch + i] = 0;
            }
        }
    }
}

fn put_pixels_8x8(input: &[i32; 64], out: &mut [i16], off: usize, pitch: usize) {
    for y in 0..8 {
        for x in 0..8 {
            out[off + y * pitch + x] = input[y * 8 + x] as i16;
        }
    }
}

/// Motion compensation of a `size` x `size` block: `buf[off..]` is set to (or, with
/// `delta`, incremented by) the reference block at `rbuf[roff..]`, interpolated per
/// `mc_type` (0 fullpel, 1 horizontal, 2 vertical, 3 both halfpel).
#[allow(clippy::too_many_arguments)]
pub fn mc(
    buf: &mut [i16],
    off: usize,
    rbuf: &[i16],
    roff: usize,
    pitch: usize,
    size: usize,
    mc_type: i32,
    delta: bool,
) {
    for i in 0..size {
        let r = roff + i * pitch;
        for j in 0..size {
            let a = rbuf[r + j] as i32;
            let v = match mc_type {
                0 => a,
                1 => (a + rbuf[r + j + 1] as i32) >> 1,
                2 => (a + rbuf[r + pitch + j] as i32) >> 1,
                _ => (a + rbuf[r + j + 1] as i32 + rbuf[r + pitch + j] as i32 + rbuf[r + pitch + j + 1] as i32) >> 2,
            };
            let d = &mut buf[off + i * pitch + j];
            *d = if delta { (*d as i32 + v) as i16 } else { v as i16 };
        }
    }
}

#[inline]
fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Recomposes a luma plane from its four 5/3 wavelet bands (all with `pitch`) into
/// `dst` (`width` x `height`, row pitch `dst_pitch`).
#[allow(clippy::too_many_arguments)]
pub fn recompose53(bands: [&[i16]; 4], pitch: usize, width: usize, height: usize, dst: &mut [u8], dst_pitch: usize) {
    let [b0, b1, b2, b3] = bands;
    let full_pitch = pitch as isize;
    let mut pitch = full_pitch;
    // Pixels at "y-1" are taken from "y" in the first row.
    let mut back_pitch: isize = 0;
    // All four band pointers move together; `base` is their common offset.
    let mut base: isize = 0;
    let at = |b: &[i16], i: isize| b[i as usize] as i32;
    let mut dst_row = 0usize;

    let mut y = 0;
    while y < height {
        if y + 2 >= height {
            pitch = 0;
        }
        let mut b0_1 = at(b0, base);
        let mut b0_2 = at(b0, base + pitch);

        let mut b1_1 = at(b1, base + back_pitch);
        let mut b1_2 = at(b1, base);
        let mut b1_3 = b1_1 - b1_2 * 6 + at(b1, base + pitch);

        let mut b2_2 = at(b2, base);
        let mut b2_3 = b2_2;
        let mut b2_5 = at(b2, base + pitch);
        let mut b2_6 = b2_5;

        let mut b3_2 = at(b3, base + back_pitch);
        let mut b3_3 = b3_2;
        let mut b3_5 = at(b3, base);
        let mut b3_6 = b3_5;
        let mut b3_8 = b3_2 - b3_5 * 6 + at(b3, base + pitch);
        let mut b3_9 = b3_8;

        let mut x = 0;
        let mut indx: isize = 0;
        while x < width {
            if x + 2 >= width {
                base -= 1;
            }
            let p = base + indx + 1;

            let b2_1 = b2_2;
            b2_2 = b2_3;
            let b2_4 = b2_5;
            b2_5 = b2_6;
            let b3_1 = b3_2;
            b3_2 = b3_3;
            let b3_4 = b3_5;
            b3_5 = b3_6;
            let b3_7 = b3_8;
            b3_8 = b3_9;

            // LL band: LPF vertically and horizontally.
            let tmp0 = b0_1;
            let tmp2 = b0_2;
            b0_1 = at(b0, p);
            b0_2 = at(b0, pitch + p);
            let tmp1 = tmp0 + b0_1;
            let mut p0 = tmp0 * 16;
            let mut p1 = tmp1 * 8;
            let mut p2 = (tmp0 + tmp2) * 8;
            let mut p3 = (tmp1 + tmp2 + b0_2) * 4;

            // HL band: HPF vertically, LPF horizontally.
            let tmp0 = b1_2;
            let tmp1 = b1_1;
            b1_2 = at(b1, p);
            b1_1 = at(b1, back_pitch + p);
            let tmp2 = tmp1 - tmp0 * 6 + b1_3;
            b1_3 = b1_1 - b1_2 * 6 + at(b1, pitch + p);
            p0 += (tmp0 + tmp1) * 8;
            p1 += (tmp0 + tmp1 + b1_1 + b1_2) * 4;
            p2 += tmp2 * 4;
            p3 += (tmp2 + b1_3) * 2;

            // LH band: LPF vertically, HPF horizontally.
            b2_3 = at(b2, p);
            b2_6 = at(b2, pitch + p);
            let tmp0 = b2_1 + b2_2;
            let tmp1 = b2_1 - b2_2 * 6 + b2_3;
            p0 += tmp0 * 8;
            p1 += tmp1 * 4;
            p2 += (tmp0 + b2_4 + b2_5) * 4;
            p3 += (tmp1 + b2_4 - b2_5 * 6 + b2_6) * 2;

            // HH band: HPF vertically and horizontally.
            b3_6 = at(b3, p);
            b3_3 = at(b3, back_pitch + p);
            let tmp0 = b3_1 + b3_4;
            let tmp1 = b3_2 + b3_5;
            let tmp2 = b3_3 + b3_6;
            b3_9 = b3_3 - b3_6 * 6 + at(b3, pitch + p);
            p0 += (tmp0 + tmp1) * 4;
            p1 += (tmp0 - tmp1 * 6 + tmp2) * 2;
            p2 += (b3_7 + b3_8) * 2;
            p3 += b3_7 - b3_8 * 6 + b3_9;

            dst[dst_row + x] = clip_u8((p0 >> 6) + 128);
            dst[dst_row + x + 1] = clip_u8((p1 >> 6) + 128);
            dst[dst_row + dst_pitch + x] = clip_u8((p2 >> 6) + 128);
            dst[dst_row + dst_pitch + x + 1] = clip_u8((p3 >> 6) + 128);

            x += 2;
            indx += 1;
        }

        dst_row += dst_pitch * 2;
        back_pitch = -pitch;
        base += pitch + 1;
        y += 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_slant_8x8_matches_dc_transform() {
        // A block with only a DC coefficient gives a flat block of (dc + 1) >> 1 in
        // both the full and the DC-only transform.
        let mut input = [0i32; 64];
        input[0] = 37;
        let mut flags = [0u8; 8];
        flags[0] = 1;
        let mut full = vec![0i16; 64];
        Transform::Slant8x8.inverse(&input, &mut full, 0, 8, &flags);
        let mut dc = vec![0i16; 64];
        Transform::Slant8x8.dc(37, &mut dc, 0, 8, 8);
        assert_eq!(full, dc);
        assert!(full.iter().all(|&v| v == 19));
    }

    #[test]
    fn halfpel_motion_compensation_averages_neighbours() {
        let pitch = 6;
        let rbuf: Vec<i16> = (0..36).map(|i| i as i16 * 2).collect();
        let mut buf = vec![0i16; 36];
        mc(&mut buf, 0, &rbuf, 0, pitch, 4, 3, false);
        // (a + right + below + below-right) / 4 = a + 1 + 6 = a + 7 in units of 2.
        assert_eq!(buf[0], 7);
        assert_eq!(buf[pitch + 1], (14 + 16 + 26 + 28) / 4);
        mc(&mut buf, 0, &rbuf, 0, pitch, 4, 0, true);
        assert_eq!(buf[0], 7);
        assert_eq!(buf[1], 2 + 9);
    }
}
