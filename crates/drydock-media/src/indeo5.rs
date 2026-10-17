// Intel Indeo Video Interactive 5 (`IV50`) decoder. Ported from FFmpeg n7.1
// libavcodec/indeo5.c and libavcodec/ivi.c (the Indeo 5 paths only: no Indeo 4
// frame types, Haar transforms or bidirectional prediction).
//
// Copyright (c) 2009 Maxim Poliakovski
//
// FFmpeg is free software; you can redistribute it and/or modify it under the terms
// of the GNU Lesser General Public License as published by the Free Software
// Foundation; either version 2.1 of the License, or (at your option) any later
// version. As permitted by section 3 of that licence, this derived file is
// distributed under the terms of the GNU General Public License version 2, as part
// of Drydock.
//
// The structure follows FFmpeg closely so the two can be compared: the decoder
// keeps the same state, makes the same checks and produces the same output.

use std::sync::OnceLock;

use crate::bits::{BitReader, VLC_BITS, Vlc};
use crate::ivi_dsp::{Transform, mc, recompose53};
use crate::ivi_tables::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid Indeo 5 data: {0}")]
    Invalid(&'static str),
    #[error("unsupported Indeo 5 feature: {0}")]
    Unsupported(&'static str),
}

type Result<T> = std::result::Result<T, Error>;

fn invalid<T>(what: &'static str) -> Result<T> {
    Err(Error::Invalid(what))
}

const FRAMETYPE_INTRA: u32 = 0;
const FRAMETYPE_INTER: u32 = 1;
const FRAMETYPE_INTER_SCAL: u32 = 2;
// Frame type 3 is a droppable P-frame (decoded like FRAMETYPE_INTER without becoming
// a reference).
const FRAMETYPE_NULL: u32 = 4;

const PIC_SIZE_ESC: u32 = 15;
const IS_PROTECTED: u8 = 0x20;

/// A decoded picture in YUV 4:1:0 (YVU9): full-resolution luma, chroma at a quarter
/// of the width and height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub chroma_width: usize,
    pub chroma_height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Frame {
    /// Converts to 24-bit RGB (BT.601, limited range), top row first.
    pub fn to_rgb24(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 3);
        for row in 0..self.height {
            let crow = (row >> 2).min(self.chroma_height - 1) * self.chroma_width;
            for col in 0..self.width {
                let ci = crow + (col >> 2).min(self.chroma_width - 1);
                let c = 298 * (self.y[row * self.width + col] as i32 - 16);
                let d = self.u[ci] as i32 - 128;
                let e = self.v[ci] as i32 - 128;
                out.push(((c + 409 * e + 128) >> 8).clamp(0, 255) as u8);
                out.push(((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8);
                out.push(((c + 516 * d + 128) >> 8).clamp(0, 255) as u8);
            }
        }
        out
    }
}

/// Builds the prefix code described by a Huffman descriptor (`xbits` per row).
fn huff_from_desc(xbits: &[u8]) -> Result<Vlc> {
    let mut codes = Vec::with_capacity(256);
    let rows = xbits.len();
    'rows: for (i, &xb) in xbits.iter().enumerate() {
        let not_last_row = (i != rows - 1) as u32;
        let prefix = ((1u32 << i) - 1) << (xb as u32 + not_last_row);
        for j in 0..1u32 << xb {
            // Some codebooks describe more than 256 codes; only 256 are allowed.
            if codes.len() >= 256 {
                break 'rows;
            }
            let bits = i as u32 + xb as u32 + not_last_row;
            if bits > VLC_BITS {
                return invalid("Huffman descriptor");
            }
            codes.push((prefix | j, bits.max(1) as u8));
        }
    }
    Ok(Vlc::new(&codes))
}

struct StaticVlcs {
    mb: Vec<Vlc>,
    blk: Vec<Vlc>,
}

fn static_vlcs() -> &'static StaticVlcs {
    static TABS: OnceLock<StaticVlcs> = OnceLock::new();
    TABS.get_or_init(|| StaticVlcs {
        mb: MB_HUFF_DESC.iter().map(|d| huff_from_desc(d).unwrap()).collect(),
        blk: BLK_HUFF_DESC.iter().map(|d| huff_from_desc(d).unwrap()).collect(),
    })
}

#[derive(Default)]
struct HuffTab {
    /// Selected table: `Some(i)` for static table `i`, `None` for the custom one.
    tab: Option<usize>,
    cust_desc: Vec<u8>,
    cust_tab: Option<Vlc>,
}

impl HuffTab {
    fn vlc(&self, blk: bool) -> &Vlc {
        let s = static_vlcs();
        match self.tab {
            Some(i) if blk => &s.blk[i],
            Some(i) => &s.mb[i],
            None => self.cust_tab.as_ref().expect("custom table is built when selected"),
        }
    }

    /// FFmpeg `ff_ivi_dec_huff_desc`.
    fn decode_desc(&mut self, gb: &mut BitReader, desc_coded: bool) -> Result<()> {
        if !desc_coded {
            self.tab = Some(7);
            return Ok(());
        }
        let sel = gb.bits(3) as usize;
        if sel != 7 {
            self.tab = Some(sel);
            return Ok(());
        }
        let num_rows = gb.bits(4) as usize;
        if num_rows == 0 {
            return invalid("empty custom Huffman table");
        }
        let xbits: Vec<u8> = (0..num_rows).map(|_| gb.bits(4) as u8).collect();
        if xbits != self.cust_desc || self.cust_tab.is_none() {
            self.cust_desc = xbits;
            match huff_from_desc(&self.cust_desc) {
                Ok(vlc) => self.cust_tab = Some(vlc),
                Err(e) => {
                    self.cust_desc.clear();
                    self.cust_tab = None;
                    return Err(e);
                }
            }
        }
        self.tab = None;
        Ok(())
    }
}

fn read_vlc(gb: &mut BitReader, vlc: &Vlc) -> Result<u32> {
    match vlc.read(gb) {
        Some(v) => Ok(v),
        None => invalid("undecodable code"),
    }
}

/// FFmpeg `IVI_TOSIGNED`: the sign is in the least significant bit.
#[inline]
fn to_signed(v: i32) -> i32 {
    -(v >> 1 ^ -(v & 1))
}

#[inline]
fn scale_mv(mv: i32, mv_scale: i32) -> i32 {
    (mv + (mv > 0) as i32 + (mv_scale - 1)) >> mv_scale
}

#[derive(Clone, Copy, Default)]
struct MbInfo {
    xpos: i16,
    ypos: i16,
    buf_offs: u32,
    ty: u8,
    cbp: u8,
    q_delta: i8,
    mv_x: i8,
    mv_y: i8,
}

#[derive(Default)]
struct Tile {
    xpos: i32,
    ypos: i32,
    width: i32,
    height: i32,
    mb_size: i32,
    is_empty: bool,
    data_size: i32,
    num_mbs: i32,
    mbs: Vec<MbInfo>,
    /// Whether this tile's macroblocks inherit from the first luma band's tile.
    has_ref: bool,
}

#[derive(Default)]
struct Band {
    plane: usize,
    band_num: usize,
    width: i32,
    height: i32,
    aheight: i32,
    pitch: i32,
    bufsize: i32,
    bufs: [Vec<i16>; 4],
    /// Index of the buffer being decoded / last decoded (FFmpeg `buf`).
    cur: Option<usize>,
    ref_idx: usize,
    is_empty: bool,
    mb_size: i32,
    blk_size: i32,
    is_halfpel: i32,
    inherit_mv: bool,
    inherit_qdelta: bool,
    qdelta_present: bool,
    glob_quant: i32,
    blk_vlc: HuffTab,
    num_corr: usize,
    corr: Vec<u8>,
    rvmap_sel: usize,
    tiles: Vec<Tile>,
    transform: Option<Transform>,
    transform_size: i32,
    is_2d_trans: bool,
    intra_base: &'static [u16],
    inter_base: &'static [u16],
    intra_scale: &'static [u8],
    inter_scale: &'static [u8],
    scan: &'static [u8],
}

#[derive(Default)]
struct Plane {
    width: u16,
    height: u16,
    bands: Vec<Band>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PicConfig {
    pic_width: u16,
    pic_height: u16,
    chroma_width: u16,
    chroma_height: u16,
    tile_width: u16,
    tile_height: u16,
    luma_bands: u8,
    chroma_bands: u8,
}

/// FFmpeg `ff_ivi_init_planes`.
fn init_planes(planes: &mut [Plane; 3], cfg: &PicConfig) -> Result<()> {
    *planes = Default::default();
    if cfg.pic_width == 0 || cfg.pic_height == 0 || cfg.luma_bands < 1 || cfg.chroma_bands < 1 {
        return invalid("picture size");
    }
    planes[0].width = cfg.pic_width;
    planes[0].height = cfg.pic_height;
    let cw = (cfg.pic_width + 3) >> 2;
    let ch = (cfg.pic_height + 3) >> 2;
    for (p, plane) in planes.iter_mut().enumerate() {
        let num_bands = if p == 0 { cfg.luma_bands } else { cfg.chroma_bands } as usize;
        if p > 0 {
            plane.width = cw;
            plane.height = ch;
        }
        let (w, h) = (plane.width as i32, plane.height as i32);
        let b_width = if num_bands == 1 { w } else { (w + 1) >> 1 };
        let b_height = if num_bands == 1 { h } else { (h + 1) >> 1 };
        // Luma band buffers are aligned on 16x16 (the largest macroblock), chroma on 8x8.
        let align = if p > 0 { 8 } else { 16 };
        let width_aligned = (b_width + align - 1) & !(align - 1);
        let height_aligned = (b_height + align - 1) & !(align - 1);
        plane.bands = (0..num_bands)
            .map(|b| Band {
                plane: p,
                band_num: b,
                width: b_width,
                height: b_height,
                pitch: width_aligned,
                aheight: height_aligned,
                bufsize: width_aligned * height_aligned,
                ..Default::default()
            })
            .collect();
    }
    Ok(())
}

#[inline]
fn mbs_per_tile(w: i32, h: i32, mb: i32) -> i32 {
    ((w + mb - 1) / mb) * ((h + mb - 1) / mb)
}

/// FFmpeg `ff_ivi_init_tiles`.
fn init_tiles(planes: &mut [Plane; 3], tile_width: i32, tile_height: i32) -> Result<()> {
    // Macroblock counts of the first luma band's tiles, which the tiles of all other
    // bands inherit motion vectors and quantiser deltas from.
    let mut ref_num_mbs: Vec<i32> = Vec::new();
    for p in 0..3 {
        let mut t_width = if p == 0 { tile_width } else { (tile_width + 3) >> 2 };
        let mut t_height = if p == 0 { tile_height } else { (tile_height + 3) >> 2 };
        if p == 0 && planes[0].bands.len() == 4 {
            if t_width % 2 != 0 || t_height % 2 != 0 {
                return Err(Error::Unsupported("odd tiles"));
            }
            t_width >>= 1;
            t_height >>= 1;
        }
        if t_width <= 0 || t_height <= 0 {
            return invalid("tile size");
        }
        for (b, band) in planes[p].bands.iter_mut().enumerate() {
            let has_ref = p != 0 || b != 0;
            band.tiles.clear();
            let mut y = 0;
            while y < band.height {
                let mut x = 0;
                while x < band.width {
                    let width = (band.width - x).min(t_width);
                    let height = (band.height - y).min(t_height);
                    let num_mbs = mbs_per_tile(width, height, band.mb_size);
                    if !has_ref {
                        ref_num_mbs.push(num_mbs);
                    } else if ref_num_mbs.get(band.tiles.len()) != Some(&num_mbs) {
                        return invalid("reference tile mismatch");
                    }
                    band.tiles.push(Tile {
                        xpos: x,
                        ypos: y,
                        width,
                        height,
                        mb_size: band.mb_size,
                        num_mbs,
                        mbs: vec![MbInfo::default(); num_mbs as usize],
                        has_ref,
                        ..Default::default()
                    });
                    x += t_width;
                }
                y += t_height;
            }
        }
    }
    Ok(())
}

/// Indeo 5 decoder state for one stream.
pub struct Decoder {
    rvmap_tabs: Vec<RvMap>,
    frame_num: u32,
    frame_type: u32,
    prev_frame_type: u32,
    is_scalable: bool,
    inter_scal: bool,
    frame_flags: u8,
    pic_conf: PicConfig,
    planes: [Plane; 3],
    buf_switch: usize,
    dst_buf: usize,
    ref_buf: usize,
    ref2_buf: usize,
    mb_vlc: HuffTab,
    gop_flags: u8,
    gop_invalid: bool,
    buf_invalid: [bool; 4],
}

impl Decoder {
    /// A decoder for pictures of the given size (from the stream's format header).
    pub fn new(width: u16, height: u16) -> Result<Self> {
        let pic_conf = PicConfig {
            pic_width: width,
            pic_height: height,
            chroma_width: (width + 3) >> 2,
            chroma_height: (height + 3) >> 2,
            tile_width: width,
            tile_height: height,
            luma_bands: 1,
            chroma_bands: 1,
        };
        let mut planes: [Plane; 3] = Default::default();
        init_planes(&mut planes, &pic_conf)?;
        Ok(Decoder {
            rvmap_tabs: RVMAP_TABS.to_vec(),
            frame_num: 0,
            frame_type: FRAMETYPE_INTRA,
            prev_frame_type: FRAMETYPE_INTRA,
            is_scalable: false,
            inter_scal: false,
            frame_flags: 0,
            pic_conf,
            planes,
            buf_switch: 0,
            dst_buf: 0,
            ref_buf: 1,
            ref2_buf: 0,
            mb_vlc: HuffTab::default(),
            gop_flags: 0,
            gop_invalid: true,
            buf_invalid: [false; 4],
        })
    }

    /// The frame number from the last picture header.
    pub fn frame_num(&self) -> u32 {
        self.frame_num
    }

    /// Decodes one compressed frame. `Ok(None)` is a null frame: the previous
    /// picture stays on display.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Frame>> {
        let mut gb = BitReader::new(data);
        self.decode_pic_hdr(&mut gb)?;
        if self.gop_invalid {
            return invalid("no valid GOP header");
        }
        if self.gop_flags & IS_PROTECTED != 0 {
            return Err(Error::Unsupported("password-protected clip"));
        }
        if self.planes[0].bands.is_empty() {
            return invalid("color planes not initialized");
        }

        self.switch_buffers();

        if self.frame_type != FRAMETYPE_NULL {
            self.buf_invalid[self.dst_buf] = true;
            for p in 0..3 {
                for b in 0..self.planes[p].bands.len() {
                    self.decode_band(&mut gb, p, b)?;
                }
            }
            self.buf_invalid[self.dst_buf] = false;
        } else {
            if self.is_scalable {
                return invalid("null frame in scalable stream");
            }
            if self.planes.iter().any(|p| p.bands[0].cur.is_none()) {
                return invalid("null frame before any picture");
            }
        }
        if self.buf_invalid[self.dst_buf] {
            return invalid("reference buffer not decoded");
        }
        if self.frame_type == FRAMETYPE_NULL {
            return Ok(None);
        }

        let width = self.planes[0].width as usize;
        let height = self.planes[0].height as usize;
        let mut y = vec![0u8; width * height];
        if self.is_scalable {
            let bands = &self.planes[0].bands;
            let buf = |i: usize| bands[i].bufs[bands[i].cur.unwrap()].as_slice();
            recompose53(
                [buf(0), buf(1), buf(2), buf(3)],
                bands[0].pitch as usize,
                width,
                height,
                &mut y,
                width,
            );
        } else {
            output_plane(&self.planes[0], &mut y);
        }
        let cw = self.planes[1].width as usize;
        let ch = self.planes[1].height as usize;
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        output_plane(&self.planes[2], &mut u);
        output_plane(&self.planes[1], &mut v);
        Ok(Some(Frame {
            width,
            height,
            chroma_width: cw,
            chroma_height: ch,
            y,
            u,
            v,
        }))
    }

    fn decode_gop_header(&mut self, gb: &mut BitReader) -> Result<()> {
        self.gop_flags = gb.bits(8) as u8;
        if self.gop_flags & 1 != 0 {
            gb.bits(16); // GOP header size
        }
        if self.gop_flags & IS_PROTECTED != 0 {
            gb.bits32(); // lock word
        }
        let tile_size = if self.gop_flags & 0x40 != 0 {
            64 << gb.bits(2)
        } else {
            0
        };
        if tile_size > 256 {
            return invalid("tile size");
        }

        // Number of wavelet bands: num_levels * 3 + 1.
        let mut pic_conf = PicConfig {
            luma_bands: gb.bits(2) as u8 * 3 + 1,
            chroma_bands: gb.bits(1) as u8 * 3 + 1,
            ..Default::default()
        };
        let is_scalable = pic_conf.luma_bands != 1 || pic_conf.chroma_bands != 1;
        if is_scalable && (pic_conf.luma_bands != 4 || pic_conf.chroma_bands != 1) {
            return Err(Error::Unsupported("scalability subdivision"));
        }

        let pic_size_indx = gb.bits(4);
        if pic_size_indx == PIC_SIZE_ESC {
            pic_conf.pic_height = gb.bits(13) as u16;
            pic_conf.pic_width = gb.bits(13) as u16;
        } else {
            let i = pic_size_indx as usize * 2;
            pic_conf.pic_height = (COMMON_PIC_SIZES[i + 1] as u16) << 2;
            pic_conf.pic_width = (COMMON_PIC_SIZES[i] as u16) << 2;
        }

        if self.gop_flags & 2 != 0 {
            return Err(Error::Unsupported("YV12 picture format"));
        }

        pic_conf.chroma_height = (pic_conf.pic_height + 3) >> 2;
        pic_conf.chroma_width = (pic_conf.pic_width + 3) >> 2;
        if tile_size == 0 {
            pic_conf.tile_height = pic_conf.pic_height;
            pic_conf.tile_width = pic_conf.pic_width;
        } else {
            pic_conf.tile_height = tile_size as u16;
            pic_conf.tile_width = tile_size as u16;
        }

        let mut blk_size_changed = false;
        if pic_conf != self.pic_conf || self.gop_invalid {
            init_planes(&mut self.planes, &pic_conf)?;
            self.pic_conf = pic_conf;
            self.is_scalable = is_scalable;
            blk_size_changed = true;
        }

        for p in 0..=1 {
            let nb = if p == 0 {
                pic_conf.luma_bands
            } else {
                pic_conf.chroma_bands
            } as usize;
            for i in 0..nb {
                let band = &mut self.planes[p].bands[i];
                band.is_halfpel = gb.bits(1) as i32;
                let mb_flag = gb.bits(1);
                let blk_size = 8 >> gb.bits(1);
                let mb_size = blk_size << (mb_flag == 0) as i32;
                if p == 0 && blk_size == 4 {
                    return Err(Error::Unsupported("4x4 luma blocks"));
                }
                // As in FFmpeg, each band overwrites the flag.
                blk_size_changed = mb_size != band.mb_size || blk_size != band.blk_size;
                if blk_size_changed {
                    band.mb_size = mb_size;
                    band.blk_size = blk_size;
                }
                if gb.bit() {
                    return Err(Error::Unsupported("extended transform info"));
                }

                let (transform, scan): (Transform, &'static [u8]) = match (p << 2) + i {
                    0 => (Transform::Slant8x8, &ZIGZAG_8X8),
                    1 => (Transform::RowSlant8, &VERTICAL_SCAN_8X8),
                    2 => (Transform::ColSlant8, &HORIZONTAL_SCAN_8X8),
                    3 => (Transform::PutPixels8x8, &HORIZONTAL_SCAN_8X8),
                    _ => (Transform::Slant4x4, &DIRECT_SCAN_4X4),
                };
                band.transform = Some(transform);
                band.scan = scan;
                band.transform_size = transform.size() as i32;
                band.is_2d_trans = transform.is_2d();
                if band.transform_size != band.blk_size {
                    return invalid("transform and block size mismatch");
                }

                let quant_mat = if p == 0 {
                    if pic_conf.luma_bands > 1 { i + 1 } else { 0 }
                } else {
                    5
                };
                if band.blk_size == 8 {
                    if quant_mat >= 5 {
                        return invalid("quant matrix");
                    }
                    band.intra_base = &BASE_QUANT_8X8_INTRA[quant_mat];
                    band.inter_base = &BASE_QUANT_8X8_INTER[quant_mat];
                    band.intra_scale = &SCALE_QUANT_8X8_INTRA[quant_mat];
                    band.inter_scale = &SCALE_QUANT_8X8_INTER[quant_mat];
                } else {
                    band.intra_base = &BASE_QUANT_4X4_INTRA;
                    band.inter_base = &BASE_QUANT_4X4_INTER;
                    band.intra_scale = &SCALE_QUANT_4X4_INTRA;
                    band.inter_scale = &SCALE_QUANT_4X4_INTER;
                }

                if gb.bits(2) != 0 {
                    return invalid("end marker missing");
                }
            }
        }

        // Copy chroma parameters into the second chroma plane.
        for i in 0..pic_conf.chroma_bands as usize {
            let [_, p1, p2] = &mut self.planes;
            let (b1, b2) = (&p1.bands[i], &mut p2.bands[i]);
            b2.width = b1.width;
            b2.height = b1.height;
            b2.mb_size = b1.mb_size;
            b2.blk_size = b1.blk_size;
            b2.is_halfpel = b1.is_halfpel;
            b2.intra_base = b1.intra_base;
            b2.inter_base = b1.inter_base;
            b2.intra_scale = b1.intra_scale;
            b2.inter_scale = b1.inter_scale;
            b2.scan = b1.scan;
            b2.transform = b1.transform;
            b2.is_2d_trans = b1.is_2d_trans;
            b2.transform_size = b1.transform_size;
        }

        if blk_size_changed {
            init_tiles(
                &mut self.planes,
                pic_conf.tile_width as i32,
                pic_conf.tile_height as i32,
            )?;
        }

        if self.gop_flags & 8 != 0 {
            if gb.bits(3) != 0 {
                return invalid("alignment bits");
            }
            if gb.bit() {
                gb.skip(24); // transparency fill color
            }
        }
        gb.align();
        gb.skip(23); // unknown meaning
        if gb.bit() {
            // GOP extension.
            while gb.bits(16) & 0x8000 != 0 {}
        }
        gb.align();
        Ok(())
    }

    fn decode_pic_hdr(&mut self, gb: &mut BitReader) -> Result<()> {
        if gb.bits(5) != 0x1f {
            return invalid("picture start code");
        }
        self.prev_frame_type = self.frame_type;
        self.frame_type = gb.bits(3);
        if self.frame_type >= 5 {
            self.frame_type = FRAMETYPE_INTRA;
            return invalid("frame type");
        }
        self.frame_num = gb.bits(8);

        if self.frame_type == FRAMETYPE_INTRA {
            if let Err(e) = self.decode_gop_header(gb) {
                self.gop_invalid = true;
                return Err(e);
            }
            self.gop_invalid = false;
        }

        if self.frame_type == FRAMETYPE_INTER_SCAL && !self.is_scalable {
            self.frame_type = FRAMETYPE_INTER;
            return invalid("scalable inter frame in non-scalable stream");
        }

        if self.frame_type != FRAMETYPE_NULL {
            self.frame_flags = gb.bits(8) as u8;
            if self.frame_flags & 1 != 0 {
                gb.bits(24); // picture header size
            }
            if self.frame_flags & 0x10 != 0 {
                gb.bits(16); // checksum
            }
            if self.frame_flags & 0x20 != 0 {
                skip_hdr_extension(gb)?;
            }
            self.mb_vlc.decode_desc(gb, self.frame_flags & 0x40 != 0)?;
            gb.skip(3); // unknown meaning
        }
        gb.align();
        Ok(())
    }

    fn switch_buffers(&mut self) {
        match self.prev_frame_type {
            FRAMETYPE_INTRA | FRAMETYPE_INTER => {
                self.buf_switch ^= 1;
                self.dst_buf = self.buf_switch;
                self.ref_buf = self.buf_switch ^ 1;
            }
            FRAMETYPE_INTER_SCAL => {
                if !self.inter_scal {
                    self.ref2_buf = 2;
                    self.inter_scal = true;
                }
                std::mem::swap(&mut self.dst_buf, &mut self.ref2_buf);
                self.ref_buf = self.ref2_buf;
            }
            _ => {}
        }
        match self.frame_type {
            FRAMETYPE_INTRA | FRAMETYPE_INTER => {
                if self.frame_type == FRAMETYPE_INTRA {
                    self.buf_switch = 0;
                }
                self.inter_scal = false;
                self.dst_buf = self.buf_switch;
                self.ref_buf = self.buf_switch ^ 1;
            }
            _ => {}
        }
    }

    /// FFmpeg `prepare_buf`.
    fn prepare_buf(luma_bands: u8, band: &mut Band, i: usize) -> Option<usize> {
        if luma_bands <= 1 && i == 2 {
            return None;
        }
        if band.bufs[i].is_empty() {
            band.bufs[i] = vec![0; band.bufsize as usize];
        }
        Some(i)
    }

    fn decode_band(&mut self, gb: &mut BitReader, p: usize, b: usize) -> Result<()> {
        let luma_mb_size = self.planes[0].bands[0].mb_size;
        let mut band = std::mem::take(&mut self.planes[p].bands[b]);
        let result = self.decode_band_inner(gb, &mut band, luma_mb_size);
        self.planes[p].bands[b] = band;
        result
    }

    fn decode_band_inner(&mut self, gb: &mut BitReader, band: &mut Band, luma_mb_size: i32) -> Result<()> {
        let luma_bands = self.pic_conf.luma_bands;
        band.cur = Self::prepare_buf(luma_bands, band, self.dst_buf);
        let Some(cur) = band.cur else {
            return invalid("band buffer");
        };
        let Some(ref_idx) = Self::prepare_buf(luma_bands, band, self.ref_buf) else {
            return invalid("reference band buffer");
        };
        if ref_idx == cur {
            return invalid("reference buffer is the decoded buffer");
        }
        band.ref_idx = ref_idx;

        self.decode_band_hdr(gb, band)?;
        if band.is_empty {
            return invalid("empty band");
        }

        let rvmap = &mut self.rvmap_tabs[band.rvmap_sel];
        apply_corrections(rvmap, &band.corr[..band.num_corr * 2], false);

        let mut buf = std::mem::take(&mut band.bufs[cur]);
        let mut tiles = std::mem::take(&mut band.tiles);
        let ref_band = &self.planes[0].bands[0];
        let ctx = MbContext {
            frame_type: self.frame_type,
            frame_flags: self.frame_flags,
            mb_vlc: &self.mb_vlc,
            luma_mb_size,
        };
        let rvmap = &self.rvmap_tabs[band.rvmap_sel];
        let mut result = Ok(());
        let mut pos = gb.pos();
        for (t, tile) in tiles.iter_mut().enumerate() {
            let ref_tile = if tile.has_ref { ref_band.tiles.get(t) } else { None };
            result = decode_tile(gb, band, &mut buf, tile, ref_tile, rvmap, &ctx, &mut pos);
            if result.is_err() {
                break;
            }
        }
        band.bufs[cur] = buf;
        band.tiles = tiles;

        // Restore the rvmap table by undoing the corrections in reverse order.
        apply_corrections(
            &mut self.rvmap_tabs[band.rvmap_sel],
            &band.corr[..band.num_corr * 2],
            true,
        );

        gb.align();
        result
    }

    fn decode_band_hdr(&mut self, gb: &mut BitReader, band: &mut Band) -> Result<()> {
        let band_flags = gb.bits(8);
        if band_flags & 1 != 0 {
            band.is_empty = true;
            return Ok(());
        }
        if self.frame_flags & 0x80 != 0 {
            gb.bits(24); // band data size
        }
        band.inherit_mv = band_flags & 2 != 0;
        band.inherit_qdelta = band_flags & 8 != 0;
        band.qdelta_present = band_flags & 4 != 0;
        if !band.qdelta_present {
            band.inherit_qdelta = true;
        }

        band.num_corr = 0;
        band.corr.clear();
        if band_flags & 0x10 != 0 {
            band.num_corr = gb.bits(8) as usize;
            if band.num_corr > 61 {
                return invalid("too many rvmap corrections");
            }
            band.corr = (0..band.num_corr * 2).map(|_| gb.bits(8) as u8).collect();
        }

        band.rvmap_sel = if band_flags & 0x40 != 0 { gb.bits(3) as usize } else { 8 };

        band.blk_vlc.decode_desc(gb, band_flags & 0x80 != 0)?;

        if gb.bit() {
            gb.bits(16); // band checksum
        }
        band.glob_quant = gb.bits(5) as i32;

        if band_flags & 0x20 != 0 {
            gb.align();
            skip_hdr_extension(gb)?;
        }
        gb.align();
        Ok(())
    }
}

fn skip_hdr_extension(gb: &mut BitReader) -> Result<()> {
    loop {
        let len = gb.bits(8) as isize;
        if 8 * len > gb.bits_left() {
            return invalid("header extension");
        }
        gb.skip(8 * len as usize);
        if len == 0 {
            return Ok(());
        }
    }
}

fn apply_corrections(rvmap: &mut RvMap, corr: &[u8], reverse: bool) {
    let mut swap = |idx1: usize, idx2: usize| {
        rvmap.runtab.swap(idx1, idx2);
        rvmap.valtab.swap(idx1, idx2);
        let (i1, i2) = (idx1 as u8, idx2 as u8);
        if i1 == rvmap.eob_sym || i2 == rvmap.eob_sym {
            rvmap.eob_sym ^= i1 ^ i2;
        }
        if i1 == rvmap.esc_sym || i2 == rvmap.esc_sym {
            rvmap.esc_sym ^= i1 ^ i2;
        }
    };
    let pairs = corr.as_chunks::<2>().0;
    if reverse {
        for &[a, b] in pairs.iter().rev() {
            swap(a as usize, b as usize);
        }
    } else {
        for &[a, b] in pairs {
            swap(a as usize, b as usize);
        }
    }
}

/// Picture-level state the macroblock decoding needs.
struct MbContext<'a> {
    frame_type: u32,
    frame_flags: u8,
    mb_vlc: &'a HuffTab,
    luma_mb_size: i32,
}

#[allow(clippy::too_many_arguments)]
fn decode_tile(
    gb: &mut BitReader,
    band: &Band,
    buf: &mut [i16],
    tile: &mut Tile,
    ref_tile: Option<&Tile>,
    rvmap: &RvMap,
    ctx: &MbContext,
    pos: &mut usize,
) -> Result<()> {
    if tile.mb_size != band.mb_size {
        return invalid("macroblock size mismatch");
    }
    let mv_scale = (ctx.luma_mb_size >> 3) - (band.mb_size >> 3);
    tile.is_empty = gb.bit();
    if tile.is_empty {
        return process_empty_tile(band, buf, tile, ref_tile, mv_scale);
    }
    tile.data_size = decode_tile_data_size(gb);
    if tile.data_size == 0 {
        return invalid("tile data size is zero");
    }
    decode_mb_info(gb, band, tile, ref_tile, ctx)?;
    decode_blocks(gb, band, buf, tile, rvmap)?;
    if (gb.pos() - *pos) >> 3 != tile.data_size as usize {
        return invalid("tile data size mismatch");
    }
    *pos += (tile.data_size as usize) << 3;
    Ok(())
}

fn decode_tile_data_size(gb: &mut BitReader) -> i32 {
    let mut len = 0;
    if gb.bit() {
        len = gb.bits(8) as i32;
        if len == 255 {
            len = gb.bits(24) as i32;
        }
    }
    gb.align();
    len
}

fn decode_mb_info(
    gb: &mut BitReader,
    band: &Band,
    tile: &mut Tile,
    ref_tile: Option<&Tile>,
    ctx: &MbContext,
) -> Result<()> {
    let pitch = band.pitch;
    let row_offset = band.mb_size * pitch;
    let mut offs = tile.ypos * pitch + tile.xpos;
    let ref_mbs = ref_tile.map(|t| t.mbs.as_slice());

    if ref_mbs.is_none() && ((band.qdelta_present && band.inherit_qdelta) || band.inherit_mv) {
        return invalid("inheritance without a reference");
    }
    if tile.num_mbs != mbs_per_tile(tile.width, tile.height, band.mb_size) {
        return invalid("tile macroblock count");
    }

    let mv_scale = (ctx.luma_mb_size >> 3) - (band.mb_size >> 3);
    let mut mv_x = 0i32;
    let mut mv_y = 0i32;
    let mb_vlc = ctx.mb_vlc.vlc(false);
    let luma0 = band.plane == 0 && band.band_num == 0;
    let read_q = |gb: &mut BitReader| -> Result<i8> {
        // FFmpeg stores the code in the int8 field before converting it.
        let v = read_vlc(gb, mb_vlc)? as i8 as i32;
        Ok(to_signed(v) as i8)
    };
    let inherit = |r: &MbInfo| -> (i8, i8) {
        if mv_scale != 0 {
            (
                scale_mv(r.mv_x as i32, mv_scale) as i8,
                scale_mv(r.mv_y as i32, mv_scale) as i8,
            )
        } else {
            (r.mv_x, r.mv_y)
        }
    };

    let mut mbi = 0usize;
    let mut y = tile.ypos;
    while y < tile.ypos + tile.height {
        let mut mb_offset = offs;
        let mut x = tile.xpos;
        while x < tile.xpos + tile.width {
            let ref_mb = ref_mbs.and_then(|r| r.get(mbi));
            let mb = &mut tile.mbs[mbi];
            mb.xpos = x as i16;
            mb.ypos = y as i16;
            mb.buf_offs = mb_offset as u32;

            if gb.bit() {
                if ctx.frame_type == FRAMETYPE_INTRA {
                    return invalid("empty macroblock in an intra picture");
                }
                mb.ty = 1; // empty macroblocks are always inter
                mb.cbp = 0;
                mb.q_delta = 0;
                if luma0 && ctx.frame_flags & 8 != 0 {
                    mb.q_delta = read_q(gb)?;
                }
                mb.mv_x = 0;
                mb.mv_y = 0;
                if band.inherit_mv
                    && let Some(r) = ref_mb
                {
                    (mb.mv_x, mb.mv_y) = inherit(r);
                }
            } else {
                if band.inherit_mv
                    && let Some(r) = ref_mb
                {
                    mb.ty = r.ty;
                } else if ctx.frame_type == FRAMETYPE_INTRA {
                    mb.ty = 0;
                } else {
                    mb.ty = gb.bits(1) as u8;
                }

                let blks_per_mb = if band.mb_size != band.blk_size { 4 } else { 1 };
                mb.cbp = gb.bits(blks_per_mb) as u8;

                mb.q_delta = 0;
                if band.qdelta_present {
                    if band.inherit_qdelta {
                        if let Some(r) = ref_mb {
                            mb.q_delta = r.q_delta;
                        }
                    } else if mb.cbp != 0 || (luma0 && ctx.frame_flags & 8 != 0) {
                        mb.q_delta = read_q(gb)?;
                    }
                }

                if mb.ty == 0 {
                    mb.mv_x = 0;
                    mb.mv_y = 0;
                } else if band.inherit_mv
                    && let Some(r) = ref_mb
                {
                    (mb.mv_x, mb.mv_y) = inherit(r);
                } else {
                    mv_y += to_signed(read_vlc(gb, mb_vlc)? as i32);
                    mv_x += to_signed(read_vlc(gb, mb_vlc)? as i32);
                    mb.mv_x = mv_x as i8;
                    mb.mv_y = mv_y as i8;
                }
            }

            let s = band.is_halfpel;
            if mb.ty != 0 {
                let (mvx, mvy) = (mb.mv_x as i32, mb.mv_y as i32);
                if x + (mvx >> s) + (y + (mvy >> s)) * pitch < 0
                    || x + ((mvx + s) >> s) + band.mb_size - 1 + (y + band.mb_size - 1 + ((mvy + s) >> s)) * pitch
                        > band.bufsize - 1
                {
                    return invalid("motion vector outside reference");
                }
            }

            mbi += 1;
            mb_offset += band.mb_size;
            x += band.mb_size;
        }
        offs += row_offset;
        y += band.mb_size;
    }
    gb.align();
    Ok(())
}

/// Checks that a block's motion vector stays inside the reference band.
fn mv_in_bounds(band: &Band, mb: &MbInfo) -> bool {
    let hp = band.is_halfpel;
    let (mvx, mvy) = (mb.mv_x as i32, mb.mv_y as i32);
    let (dmv_x, dmv_y) = (mvx >> hp, mvy >> hp);
    let (cx, cy) = (mvx & hp, mvy & hp);
    let (x, y) = (mb.xpos as i32, mb.ypos as i32);
    !(x + dmv_x < 0
        || x + dmv_x + band.mb_size + cx > band.pitch
        || y + dmv_y < 0
        || y + dmv_y + band.mb_size + cy > band.aheight)
}

/// FFmpeg `ivi_mc` for unidirectional prediction.
fn ivi_mc(band: &Band, buf: &mut [i16], offs: i32, mv_x: i32, mv_y: i32, mc_type: i32, delta: bool) -> Result<()> {
    let pitch = band.pitch;
    let ref_offs = offs + mv_y * pitch + mv_x;
    let buf_size = pitch * band.aheight;
    let min_size = pitch * (band.blk_size - 1) + band.blk_size;
    let ref_size = (mc_type > 1) as i32 * pitch + (mc_type & 1);
    if offs < 0 || ref_offs < 0 || buf_size - min_size < offs || buf_size - min_size - ref_size < ref_offs {
        return invalid("motion compensation outside the band");
    }
    let rbuf = &band.bufs[band.ref_idx];
    mc(
        buf,
        offs as usize,
        rbuf,
        ref_offs as usize,
        pitch as usize,
        band.blk_size as usize,
        mc_type,
        delta,
    );
    Ok(())
}

fn decode_blocks(gb: &mut BitReader, band: &Band, buf: &mut [i16], tile: &Tile, rvmap: &RvMap) -> Result<()> {
    let mut prev_dc = 0i32;
    let blk_size = band.blk_size;
    let num_blocks = if band.mb_size != blk_size { 4 } else { 1 };
    let mut mc_type = 0i32;
    let mut mv_x = 0i32;
    let mut mv_y = 0i32;
    let transform = band.transform.ok_or(Error::Invalid("no transform"))?;

    for mb in &tile.mbs[..tile.num_mbs as usize] {
        let is_intra = mb.ty == 0;
        let mut cbp = mb.cbp;
        let mut buf_offs = mb.buf_offs as i32;

        let quant = (band.glob_quant + mb.q_delta as i32).clamp(0, 23) as usize;
        let scale_tab = if is_intra { band.intra_scale } else { band.inter_scale };
        let quant = scale_tab[quant] as u32;

        if !is_intra {
            mv_x = mb.mv_x as i32;
            mv_y = mb.mv_y as i32;
            if band.is_halfpel != 0 {
                mc_type = ((mv_y & 1) << 1) | (mv_x & 1);
                mv_x >>= 1;
                mv_y >>= 1;
            }
            if !mv_in_bounds(band, mb) {
                return invalid("motion vector out of bounds");
            }
        }

        for blk in 0..num_blocks {
            if blk & 1 != 0 {
                buf_offs += blk_size;
            } else if blk == 2 {
                buf_offs -= blk_size;
                buf_offs += blk_size * band.pitch;
            }

            if cbp & 1 != 0 {
                decode_coded_block(
                    gb,
                    band,
                    rvmap,
                    buf,
                    transform,
                    mv_x,
                    mv_y,
                    &mut prev_dc,
                    is_intra,
                    mc_type,
                    quant,
                    buf_offs,
                )?;
            } else {
                let buf_size = band.pitch * band.aheight - buf_offs;
                let min_size = (blk_size - 1) * band.pitch + blk_size;
                if min_size > buf_size {
                    return invalid("block outside the band");
                }
                if is_intra {
                    transform.dc(prev_dc, buf, buf_offs as usize, band.pitch as usize, blk_size as usize);
                } else {
                    ivi_mc(band, buf, buf_offs, mv_x, mv_y, mc_type, false)?;
                }
            }
            cbp >>= 1;
        }
    }
    gb.align();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_coded_block(
    gb: &mut BitReader,
    band: &Band,
    rvmap: &RvMap,
    buf: &mut [i16],
    transform: Transform,
    mv_x: i32,
    mv_y: i32,
    prev_dc: &mut i32,
    is_intra: bool,
    mc_type: i32,
    quant: u32,
    offs: i32,
) -> Result<()> {
    let base_tab = if is_intra { band.intra_base } else { band.inter_base };
    let blk_size = band.blk_size;
    let num_coeffs = blk_size * blk_size;
    let col_mask = (blk_size - 1) as usize;
    let mut scan_pos: i32 = -1;
    let min_size = band.pitch * (band.transform_size - 1) + band.transform_size;
    let buf_size = band.pitch * band.aheight - offs;
    if min_size > buf_size {
        return invalid("block outside the band");
    }

    let vlc = band.blk_vlc.vlc(true);
    let mut trvec = [0i32; 64];
    let mut col_flags = [0u8; 8];
    let mut sym = 0u32;
    while scan_pos <= num_coeffs {
        sym = read_vlc(gb, vlc)?;
        if sym == rvmap.eob_sym as u32 {
            break;
        }
        let (run, val);
        if sym == rvmap.esc_sym as u32 {
            // Escape: run and value coded explicitly with three codes.
            run = read_vlc(gb, vlc)? as i32 + 1;
            let lo = read_vlc(gb, vlc)?;
            let hi = read_vlc(gb, vlc)?;
            val = to_signed(((hi << 6) | lo) as i32);
        } else {
            if sym >= 256 {
                return invalid("block symbol");
            }
            run = rvmap.runtab[sym as usize] as i32;
            val = rvmap.valtab[sym as usize] as i32;
        }

        scan_pos += run;
        if scan_pos >= num_coeffs || scan_pos < 0 {
            break;
        }
        let pos = band.scan[scan_pos as usize] as usize;

        let q = (base_tab[pos] as u32 * quant) >> 9;
        let mut val = val;
        if q > 1 {
            let sign = if val > 0 { 1 } else { -1 };
            val = val
                .wrapping_mul(q as i32)
                .wrapping_add(sign * ((((q ^ 1) - 1) >> 1) as i32));
        }
        trvec[pos] = val;
        col_flags[pos & col_mask] |= (val != 0) as u8;
    }

    if scan_pos < 0 || (scan_pos >= num_coeffs && sym != rvmap.eob_sym as u32) {
        return invalid("corrupt block data");
    }

    // Undo the DC prediction of intra blocks.
    if is_intra && band.is_2d_trans {
        *prev_dc = prev_dc.wrapping_add(trvec[0]);
        trvec[0] = *prev_dc;
        col_flags[0] |= (*prev_dc != 0) as u8;
    }

    transform.inverse(&trvec, buf, offs as usize, band.pitch as usize, &col_flags);

    if !is_intra {
        ivi_mc(band, buf, offs, mv_x, mv_y, mc_type, true)?;
    }
    Ok(())
}

fn process_empty_tile(
    band: &Band,
    buf: &mut [i16],
    tile: &mut Tile,
    ref_tile: Option<&Tile>,
    mv_scale: i32,
) -> Result<()> {
    let mb_size = band.mb_size;
    let pitch = band.pitch;
    let is_halfpel = band.is_halfpel;
    let clear_first = !band.qdelta_present && band.plane == 0 && band.band_num == 0;
    if tile.num_mbs != mbs_per_tile(tile.width, tile.height, mb_size) {
        return invalid("tile macroblock count");
    }

    let ref_mbs = ref_tile.map(|t| t.mbs.as_slice());
    let mut offs = tile.ypos * pitch + tile.xpos;
    let row_offset = mb_size * pitch;
    let mut need_mc = false;
    let mut mbi = 0usize;
    let mut y = tile.ypos;
    while y < tile.ypos + tile.height {
        let mut mb_offset = offs;
        let mut x = tile.xpos;
        while x < tile.xpos + tile.width {
            let mb = &mut tile.mbs[mbi];
            mb.xpos = x as i16;
            mb.ypos = y as i16;
            mb.buf_offs = mb_offset as u32;
            mb.ty = 1;
            mb.cbp = 0;
            if clear_first {
                mb.q_delta = band.glob_quant as i8;
                mb.mv_x = 0;
                mb.mv_y = 0;
            }
            if let Some(r) = ref_mbs.and_then(|r| r.get(mbi)) {
                if band.inherit_qdelta {
                    mb.q_delta = r.q_delta;
                }
                if band.inherit_mv {
                    if mv_scale != 0 {
                        mb.mv_x = scale_mv(r.mv_x as i32, mv_scale) as i8;
                        mb.mv_y = scale_mv(r.mv_y as i32, mv_scale) as i8;
                    } else {
                        mb.mv_x = r.mv_x;
                        mb.mv_y = r.mv_y;
                    }
                    need_mc |= mb.mv_x != 0 || mb.mv_y != 0;
                    let hp = is_halfpel;
                    let (mvx, mvy) = (mb.mv_x as i32, mb.mv_y as i32);
                    let (dmv_x, dmv_y, cx, cy) = (mvx >> hp, mvy >> hp, mvx & hp, mvy & hp);
                    if x + dmv_x < 0
                        || x + dmv_x + mb_size + cx > pitch
                        || y + dmv_y < 0
                        || y + dmv_y + mb_size + cy > band.aheight
                    {
                        return invalid("motion vector out of bounds");
                    }
                }
            }
            mbi += 1;
            mb_offset += mb_size;
            x += mb_size;
        }
        offs += row_offset;
        y += mb_size;
    }

    if band.inherit_mv && need_mc {
        let num_blocks = if mb_size != band.blk_size { 4 } else { 1 };
        for mb in &tile.mbs[..tile.num_mbs as usize] {
            let (mut mv_x, mut mv_y) = (mb.mv_x as i32, mb.mv_y as i32);
            let mc_type = if band.is_halfpel == 0 {
                0
            } else {
                let t = ((mv_y & 1) << 1) | (mv_x & 1);
                mv_x >>= 1;
                mv_y >>= 1;
                t
            };
            for blk in 0..num_blocks {
                let offs = mb.buf_offs as i32 + band.blk_size * ((blk & 1) + (blk & 2 != 0) as i32 * pitch);
                ivi_mc(band, buf, offs, mv_x, mv_y, mc_type, false)?;
            }
        }
    } else {
        // Copy the reference tile.
        let rbuf = &band.bufs[band.ref_idx];
        for row in 0..tile.height {
            let o = ((tile.ypos + row) * pitch + tile.xpos) as usize;
            buf[o..o + tile.width as usize].copy_from_slice(&rbuf[o..o + tile.width as usize]);
        }
    }
    Ok(())
}

/// FFmpeg `ivi_output_plane`: adds the bias of 128 back and clips.
fn output_plane(plane: &Plane, dst: &mut [u8]) {
    let band = &plane.bands[0];
    let Some(cur) = band.cur else { return };
    let src = &band.bufs[cur];
    let (w, h, pitch) = (plane.width as usize, plane.height as usize, band.pitch as usize);
    for y in 0..h {
        for x in 0..w {
            dst[y * w + x] = (src[y * pitch + x] as i32 + 128).clamp(0, 255) as u8;
        }
    }
}
