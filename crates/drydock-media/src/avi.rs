//! AVI (RIFF `AVI `) container parsing: headers, stream formats and the location of
//! every stream's data chunks.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not an AVI file")]
    NotAvi,
    #[error("malformed AVI: {0}")]
    Malformed(&'static str),
}

type Result<T> = std::result::Result<T, Error>;

/// The main AVI header (`avih`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MainHeader {
    pub micro_sec_per_frame: u32,
    pub flags: u32,
    pub total_frames: u32,
    pub streams: u32,
    pub width: u32,
    pub height: u32,
}

/// A stream's kind (`strh` fccType).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Other([u8; 4]),
}

/// One data chunk of a stream: `size` bytes at `offset` in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub offset: usize,
    pub size: usize,
    pub keyframe: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub kind: Kind,
    pub handler: [u8; 4],
    pub flags: u32,
    pub scale: u32,
    pub rate: u32,
    pub start: u32,
    pub length: u32,
    pub suggested_buffer_size: u32,
    /// The raw stream format (`strf`): a BITMAPINFOHEADER for video, a WAVEFORMATEX
    /// for audio.
    pub format: Vec<u8>,
    pub chunks: Vec<Chunk>,
}

/// The fields of a BITMAPINFOHEADER.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BitmapInfo {
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bit_count: u16,
    pub compression: [u8; 4],
    pub size_image: u32,
}

/// The fields of a WAVEFORMATEX.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveFormat {
    pub format_tag: u16,
    pub channels: u16,
    pub samples_per_sec: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    d.get(o..o + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn fourcc(d: &[u8], o: usize) -> [u8; 4] {
    d.get(o..o + 4).map_or([0; 4], |b| [b[0], b[1], b[2], b[3]])
}

impl Stream {
    pub fn bitmap_info(&self) -> Option<BitmapInfo> {
        let f = &self.format;
        (self.kind == Kind::Video && f.len() >= 40).then(|| BitmapInfo {
            width: u32_at(f, 4) as i32,
            height: u32_at(f, 8) as i32,
            planes: u16_at(f, 12),
            bit_count: u16_at(f, 14),
            compression: fourcc(f, 16),
            size_image: u32_at(f, 20),
        })
    }

    pub fn wave_format(&self) -> Option<WaveFormat> {
        let f = &self.format;
        (self.kind == Kind::Audio && f.len() >= 14).then(|| WaveFormat {
            format_tag: u16_at(f, 0),
            channels: u16_at(f, 2),
            samples_per_sec: u32_at(f, 4),
            avg_bytes_per_sec: u32_at(f, 8),
            block_align: u16_at(f, 12),
            bits_per_sample: u16_at(f, 14),
        })
    }

    /// Stream duration in microseconds (`length` units of `scale / rate` seconds).
    pub fn duration_us(&self) -> u64 {
        if self.rate == 0 {
            return 0;
        }
        self.length as u64 * self.scale as u64 * 1_000_000 / self.rate as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Avi {
    pub header: MainHeader,
    pub streams: Vec<Stream>,
}

const AVIIF_KEYFRAME: u32 = 0x10;

/// The stream number of a data chunk id such as `00dc` or `01wb`.
fn chunk_stream(id: [u8; 4]) -> Option<usize> {
    let d = |c: u8| (c as char).to_digit(10);
    Some((d(id[0])? * 10 + d(id[1])?) as usize)
}

/// Iterates over the RIFF chunks in `d[start..end]`: (id, data offset, data size).
fn chunks(d: &[u8], start: usize, end: usize) -> impl Iterator<Item = ([u8; 4], usize, usize)> + '_ {
    let end = end.min(d.len());
    let mut off = start;
    std::iter::from_fn(move || {
        if off + 8 > end {
            return None;
        }
        let id = fourcc(d, off);
        let size = u32_at(d, off + 4) as usize;
        let data = off + 8;
        let size = size.min(end - data);
        off = data + size + (size & 1);
        Some((id, data, size))
    })
}

impl Avi {
    pub fn parse(d: &[u8]) -> Result<Avi> {
        if d.len() < 12 || &d[0..4] != b"RIFF" || &d[8..12] != b"AVI " {
            return Err(Error::NotAvi);
        }
        let riff_end = (8 + u32_at(d, 4) as usize).min(d.len());
        let mut header = None;
        let mut streams = Vec::new();
        let mut movi = None;
        let mut idx1 = None;
        for (id, off, size) in chunks(d, 12, riff_end) {
            match &id {
                b"LIST" if size >= 4 => match &fourcc(d, off) {
                    b"hdrl" => {
                        for (id, off, size) in chunks(d, off + 4, off + size) {
                            match &id {
                                b"avih" => header = Some(parse_avih(&d[off..off + size])),
                                b"LIST" if size >= 4 && &fourcc(d, off) == b"strl" => {
                                    streams.push(parse_strl(d, off + 4, off + size)?);
                                }
                                _ => {}
                            }
                        }
                    }
                    b"movi" => movi = Some((off, off + size)),
                    _ => {}
                },
                b"idx1" => idx1 = Some((off, size)),
                _ => {}
            }
        }
        let header = header.ok_or(Error::Malformed("no avih header"))?;
        let (movi_start, movi_end) = movi.ok_or(Error::Malformed("no movi list"))?;

        let mut indexed = false;
        if let Some((off, size)) = idx1 {
            indexed = read_idx1(d, &d[off..off + size], movi_start, &mut streams);
        }
        if !indexed {
            for s in &mut streams {
                s.chunks.clear();
            }
            scan_movi(d, movi_start + 4, movi_end, &mut streams);
        }
        Ok(Avi { header, streams })
    }
}

fn parse_avih(h: &[u8]) -> MainHeader {
    MainHeader {
        micro_sec_per_frame: u32_at(h, 0),
        flags: u32_at(h, 12),
        total_frames: u32_at(h, 16),
        streams: u32_at(h, 24),
        width: u32_at(h, 32),
        height: u32_at(h, 36),
    }
}

fn parse_strl(d: &[u8], start: usize, end: usize) -> Result<Stream> {
    let mut stream = None;
    let mut format = Vec::new();
    for (id, off, size) in chunks(d, start, end) {
        match &id {
            b"strh" if size >= 36 => {
                let h = &d[off..off + size];
                let kind = match &fourcc(h, 0) {
                    b"vids" => Kind::Video,
                    b"auds" => Kind::Audio,
                    other => Kind::Other(*other),
                };
                stream = Some(Stream {
                    kind,
                    handler: fourcc(h, 4),
                    flags: u32_at(h, 8),
                    scale: u32_at(h, 20),
                    rate: u32_at(h, 24),
                    start: u32_at(h, 28),
                    length: u32_at(h, 32),
                    suggested_buffer_size: u32_at(h, 36),
                    format: Vec::new(),
                    chunks: Vec::new(),
                });
            }
            b"strf" => format = d[off..off + size].to_vec(),
            _ => {}
        }
    }
    let mut stream = stream.ok_or(Error::Malformed("stream without strh"))?;
    stream.format = format;
    Ok(stream)
}

/// Fills the streams' chunk lists from an `idx1` index. Offsets are relative to the
/// `movi` list type (the usual case) or absolute; the first entry tells which.
fn read_idx1(d: &[u8], idx: &[u8], movi_start: usize, streams: &mut [Stream]) -> bool {
    let entries: Vec<_> = idx
        .as_chunks::<16>().0.iter()
        .map(|e| {
            (
                fourcc(e, 0),
                u32_at(e, 4),
                u32_at(e, 8) as usize,
                u32_at(e, 12) as usize,
            )
        })
        .collect();
    let Some(&(first_id, _, first_off, _)) = entries.first() else {
        return false;
    };
    let base = if fourcc(d, movi_start + first_off) == first_id {
        movi_start
    } else if fourcc(d, first_off) == first_id {
        0
    } else {
        return false;
    };
    for (id, flags, off, size) in entries {
        let Some(s) = chunk_stream(id).and_then(|n| streams.get_mut(n)) else {
            continue;
        };
        let offset = base + off + 8;
        if offset + size > d.len() {
            return false;
        }
        s.chunks.push(Chunk {
            offset,
            size,
            keyframe: flags & AVIIF_KEYFRAME != 0,
        });
    }
    true
}

fn scan_movi(d: &[u8], start: usize, end: usize, streams: &mut [Stream]) {
    for (id, off, size) in chunks(d, start, end) {
        if &id == b"LIST" && size >= 4 {
            scan_movi(d, off + 4, off + size, streams);
        } else if let Some(s) = chunk_stream(id).and_then(|n| streams.get_mut(n)) {
            s.chunks.push(Chunk {
                offset: off,
                size,
                keyframe: s.chunks.is_empty(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        if data.len() & 1 == 1 {
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

    fn sample(with_index: bool) -> Vec<u8> {
        let mut avih = vec![0u8; 56];
        avih[0..4].copy_from_slice(&66666u32.to_le_bytes());
        avih[16..20].copy_from_slice(&2u32.to_le_bytes());
        avih[32..36].copy_from_slice(&320u32.to_le_bytes());
        avih[36..40].copy_from_slice(&240u32.to_le_bytes());
        let mut strh = vec![0u8; 56];
        strh[0..4].copy_from_slice(b"vids");
        strh[4..8].copy_from_slice(b"IV50");
        strh[20..24].copy_from_slice(&1u32.to_le_bytes());
        strh[24..28].copy_from_slice(&15u32.to_le_bytes());
        strh[32..36].copy_from_slice(&2u32.to_le_bytes());
        let mut strf = vec![0u8; 40];
        strf[4..8].copy_from_slice(&320u32.to_le_bytes());
        strf[8..12].copy_from_slice(&240u32.to_le_bytes());
        strf[16..20].copy_from_slice(b"IV50");
        let hdrl = list(
            b"hdrl",
            &[
                chunk(b"avih", &avih),
                list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &strf)]),
            ],
        );
        let movi = list(b"movi", &[chunk(b"00dc", &[1, 2, 3]), chunk(b"00dc", &[4, 5])]);
        let mut body = b"AVI ".to_vec();
        body.extend(&hdrl);
        let movi_start = 8 + body.len() + 8;
        body.extend(&movi);
        if with_index {
            let mut idx = Vec::new();
            for (flags, off, size) in [(0x10u32, 4u32, 3u32), (0, 16, 2)] {
                idx.extend(b"00dc");
                idx.extend(flags.to_le_bytes());
                idx.extend(off.to_le_bytes());
                idx.extend(size.to_le_bytes());
            }
            body.extend(chunk(b"idx1", &idx));
        }
        let file = chunk(b"RIFF", &body);
        assert_eq!(&file[movi_start..movi_start + 4], b"movi");
        file
    }

    #[test]
    fn parses_headers_and_chunk_locations() {
        for with_index in [true, false] {
            let file = sample(with_index);
            let avi = Avi::parse(&file).unwrap();
            assert_eq!(avi.header.micro_sec_per_frame, 66666);
            assert_eq!((avi.header.width, avi.header.height), (320, 240));
            let s = &avi.streams[0];
            assert_eq!(s.kind, Kind::Video);
            assert_eq!(s.duration_us(), 133_333);
            assert_eq!(s.bitmap_info().unwrap().compression, *b"IV50");
            let data: Vec<&[u8]> = s.chunks.iter().map(|c| &file[c.offset..c.offset + c.size]).collect();
            assert_eq!(data, [&[1u8, 2, 3][..], &[4, 5][..]], "index: {with_index}");
            assert!(s.chunks[0].keyframe);
            assert!(!s.chunks[1].keyframe);
        }
    }

    #[test]
    fn rejects_other_riff_files() {
        assert_eq!(Avi::parse(&chunk(b"RIFF", b"WAVEfmt ")), Err(Error::NotAvi));
    }
}
