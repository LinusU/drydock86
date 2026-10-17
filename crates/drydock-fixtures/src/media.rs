//! Self-authored media files for tests: Indeo 5 frames and AVI files.
//!
//! The Indeo 5 encoder writes intra frames of a single colour, the smallest valid
//! pictures: one band per plane, one tile per band, and a single coded block per band
//! that carries the DC value (every other block repeats it through DC prediction).

/// Little-endian bit writer (least significant bit of each byte first).
struct Bits {
    out: Vec<u8>,
    n: usize,
}

impl Bits {
    fn new() -> Self {
        Bits { out: Vec::new(), n: 0 }
    }
    fn put(&mut self, v: u32, bits: u32) {
        for i in 0..bits {
            if self.n.is_multiple_of(8) {
                self.out.push(0);
            }
            if v >> i & 1 != 0 {
                *self.out.last_mut().unwrap() |= 1 << (self.n % 8);
            }
            self.n += 1;
        }
    }
    /// A prefix code, most significant bit first.
    fn code(&mut self, (code, len): (u32, u32)) {
        for i in (0..len).rev() {
            self.put(code >> i & 1, 1);
        }
    }
    fn align(&mut self) {
        self.n = self.n.div_ceil(8) * 8;
    }
    fn bytes(&self) -> usize {
        self.n.div_ceil(8)
    }
}

/// The default block codebook (static table 7, rows of `xbits`) as (code, length) by
/// symbol, built as Indeo describes Huffman tables.
fn block_codes() -> Vec<(u32, u32)> {
    let xbits = [3u32, 4, 4, 5, 5, 5, 6, 5, 5];
    let mut codes = Vec::new();
    for (i, &xb) in xbits.iter().enumerate() {
        let not_last = (i != xbits.len() - 1) as u32;
        let prefix = ((1u32 << i) - 1) << (xb + not_last);
        for j in 0..1u32 << xb {
            if codes.len() < 256 {
                codes.push((prefix | j, i as u32 + xb + not_last));
            }
        }
    }
    codes
}

/// Escape symbol and end-of-block symbol of the default run-value table (table 8).
const ESC_SYM: usize = 11;
const EOB_SYM: usize = 4;

/// A tile of `mbs` macroblocks whose first block carries DC coefficient `dc`.
fn tile(b: &mut Bits, mbs: usize, blocks_per_mb: u32, dc: i32) {
    let codes = block_codes();
    // Macroblock info: not empty, coded block pattern (only the very first block).
    let mut info = Bits::new();
    for m in 0..mbs {
        info.put(0, 1);
        info.put((m == 0) as u32, blocks_per_mb);
    }
    info.align();
    // The coded block: escape (run 1, value dc), then end of block.
    let mut blk = Bits::new();
    let u = if dc > 0 {
        2 * dc as u32 - 1
    } else {
        2 * dc.unsigned_abs()
    };
    blk.code(codes[ESC_SYM]);
    blk.code(codes[0]); // run - 1
    blk.code(codes[(u & 63) as usize]);
    blk.code(codes[(u >> 6) as usize]);
    blk.code(codes[EOB_SYM]);
    blk.align();
    // Tile header: not empty, data size (bytes from the tile start), aligned.
    let size = 2 + info.bytes() + blk.bytes();
    assert!(size < 255, "fixture tile too large");
    b.put(0, 1);
    b.put(1, 1);
    b.put(size as u32, 8);
    b.align();
    b.out.extend_from_slice(&info.out);
    b.n += info.bytes() * 8;
    b.out.extend_from_slice(&blk.out);
    b.n += blk.bytes() * 8;
}

/// One Indeo 5 (`IV50`) intra frame of `width` x `height` pixels, all of colour
/// (`y`, `u`, `v`). Dimensions must be multiples of 16.
pub fn indeo5_flat_frame(width: u32, height: u32, frame_num: u32, (y, u, v): (u8, u8, u8)) -> Vec<u8> {
    assert!(width.is_multiple_of(16) && height.is_multiple_of(16));
    let mut b = Bits::new();
    // Picture start code, intra frame, frame number.
    b.put(0x1f, 5);
    b.put(0, 3);
    b.put(frame_num & 0xff, 8);
    // GOP header: no flags, one luma and one chroma band, explicit size.
    b.put(0, 8);
    b.put(0, 2);
    b.put(0, 1);
    b.put(15, 4);
    b.put(height, 13);
    b.put(width, 13);
    // Luma band: fullpel, 16x16 macroblocks of 8x8 blocks.
    b.put(0, 1); // halfpel
    b.put(0, 1); // macroblock = 2 x block
    b.put(0, 1); // 8x8 blocks
    b.put(0, 1); // no extended transform
    b.put(0, 2); // end marker
    // Chroma band: 4x4 macroblocks of one 4x4 block.
    b.put(0, 1);
    b.put(1, 1);
    b.put(1, 1);
    b.put(0, 1);
    b.put(0, 2);
    b.align();
    b.put(0, 23);
    b.put(0, 1); // no GOP extension
    b.align();
    // Picture header: no flags, default macroblock codebook.
    b.put(0, 8);
    b.put(0, 3);
    b.align();

    let (cw, ch) = (width.div_ceil(4), height.div_ceil(4));
    // DC coefficients reproducing the colour: pixel = ((dc + 1) >> 1) + 128.
    let dc = |c: u8| 2 * (c as i32 - 128);
    for (plane, value, mbs, bpm) in [
        (0, y, (width / 16 * (height / 16)) as usize, 4),
        (1, v, (cw.div_ceil(4) * ch.div_ceil(4)) as usize, 1),
        (2, u, (cw.div_ceil(4) * ch.div_ceil(4)) as usize, 1),
    ] {
        let _ = plane;
        // Band header: no flags, no checksum, global quantiser 0.
        b.put(0, 8);
        b.put(0, 1);
        b.put(0, 5);
        b.align();
        tile(&mut b, mbs, bpm, dc(value));
        b.align();
    }
    b.out.truncate(b.bytes());
    b.out
}

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], items: &[Vec<u8>]) -> Vec<u8> {
    let mut data = kind.to_vec();
    for i in items {
        data.extend(i);
    }
    chunk(b"LIST", &data)
}

fn le(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// 8-bit mono PCM sound track for [`avi`].
pub struct Pcm<'a> {
    pub rate: u32,
    pub samples: &'a [u8],
}

/// An AVI file with an Indeo 5 video stream of `frames` at `fps`, and optionally an
/// interleaved PCM sound track, with an idx1 index.
pub fn avi(width: u32, height: u32, fps: u32, frames: &[Vec<u8>], audio: Option<Pcm>) -> Vec<u8> {
    let n = frames.len() as u32;
    let streams = 1 + audio.is_some() as u32;
    let avih = le(&[1_000_000 / fps, 0, 0, 0x10, n, 0, streams, 0, width, height, 0, 0, 0, 0]);
    let mut strh_v = b"vidsIV50".to_vec();
    strh_v.extend(le(&[0, 0, 0, 1, fps, 0, n, 0, u32::MAX, 0]));
    strh_v.extend([0u8; 8]);
    let mut strf_v = le(&[40, width, height]);
    strf_v.extend(1u16.to_le_bytes());
    strf_v.extend(24u16.to_le_bytes());
    strf_v.extend(b"IV50");
    strf_v.extend(le(&[width * height * 3, 0, 0, 0, 0]));
    let mut hdrl = vec![
        chunk(b"avih", &avih),
        list(b"strl", &[chunk(b"strh", &strh_v), chunk(b"strf", &strf_v)]),
    ];
    if let Some(a) = &audio {
        let mut strh_a = b"auds\0\0\0\0".to_vec();
        strh_a.extend(le(&[0, 0, 0, 1, a.rate, 0, a.samples.len() as u32, 0, u32::MAX, 1]));
        strh_a.extend([0u8; 8]);
        let mut strf_a = Vec::new();
        strf_a.extend(1u16.to_le_bytes()); // PCM
        strf_a.extend(1u16.to_le_bytes()); // mono
        strf_a.extend(le(&[a.rate, a.rate]));
        strf_a.extend(1u16.to_le_bytes()); // block align
        strf_a.extend(8u16.to_le_bytes()); // bits
        hdrl.push(list(b"strl", &[chunk(b"strh", &strh_a), chunk(b"strf", &strf_a)]));
    }
    // Interleave: each video frame followed by its share of the sound.
    let mut items = Vec::new();
    let mut index = Vec::new();
    let mut off = 4u32;
    let mut push = |items: &mut Vec<Vec<u8>>, id: &[u8; 4], data: &[u8], key: bool| {
        let c = chunk(id, data);
        index.extend(id);
        index.extend(le(&[if key { 0x10 } else { 0 }, off, data.len() as u32]));
        off += c.len() as u32;
        items.push(c);
    };
    for (i, f) in frames.iter().enumerate() {
        push(&mut items, b"00dc", f, true);
        if let Some(a) = &audio {
            let per = a.samples.len().div_ceil(frames.len());
            let s = &a.samples[(i * per).min(a.samples.len())..((i + 1) * per).min(a.samples.len())];
            if !s.is_empty() {
                push(&mut items, b"01wb", s, true);
            }
        }
    }
    let mut body = b"AVI ".to_vec();
    body.extend(list(b"hdrl", &hdrl));
    body.extend(list(b"movi", &items));
    body.extend(chunk(b"idx1", &index));
    chunk(b"RIFF", &body)
}
