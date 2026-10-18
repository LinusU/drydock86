//! msacm32: the Audio Compression Manager with the codecs Windows installs for
//! Microsoft ADPCM and IMA ADPCM (decoding to 16-bit PCM) and the PCM converter
//! (identity conversions only).

use crate::api::*;
use crate::export;
use crate::session::Session;
use drydock_media::adpcm;
use serde::{Deserialize, Serialize};

const MMSYSERR_INVALHANDLE: u32 = 5;
const MMSYSERR_NOTSUPPORTED: u32 = 8;
const MMSYSERR_INVALFLAG: u32 = 10;
const MMSYSERR_INVALPARAM: u32 = 11;
const ACMERR_NOTPOSSIBLE: u32 = 512;
const ACMERR_UNPREPARED: u32 = 514;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_ADPCM: u16 = 2;
const WAVE_FORMAT_IMA_ADPCM: u16 = 0x11;

const ACMSTREAMHEADER_STATUSF_DONE: u32 = 0x0001_0000;
const ACMSTREAMHEADER_STATUSF_PREPARED: u32 = 0x0002_0000;

/// The largest format a codec here describes: MS ADPCM with its seven coefficients.
const MAX_FORMAT_SIZE: u32 = 18 + 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveFmt {
    pub tag: u16,
    pub channels: u16,
    pub rate: u32,
    pub avg_bytes: u32,
    pub block_align: u16,
    pub bits: u16,
    /// MS ADPCM samples per block and coefficients.
    pub samples_per_block: u16,
    pub coefs: Vec<(i16, i16)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcmStream {
    pub src: WaveFmt,
    pub dst: WaveFmt,
}

fn read_wfx(s: &Session, p: u32) -> Result<WaveFmt, ApiError> {
    let b = s.mem.read_vec(p, 16)?;
    let g16 = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let g32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let tag = g16(0);
    let mut f = WaveFmt {
        tag,
        channels: g16(2),
        rate: g32(4),
        avg_bytes: g32(8),
        block_align: g16(12),
        bits: g16(14),
        samples_per_block: 0,
        coefs: Vec::new(),
    };
    if tag != WAVE_FORMAT_PCM {
        let cb = s.mem.read_u16(p + 16)?;
        if cb >= 2 {
            f.samples_per_block = s.mem.read_u16(p + 18)?;
        }
        if tag == WAVE_FORMAT_ADPCM && cb >= 4 {
            let n = s.mem.read_u16(p + 20)?.min(256) as u32;
            for i in 0..n {
                let c1 = s.mem.read_u16(p + 22 + 4 * i)? as i16;
                let c2 = s.mem.read_u16(p + 24 + 4 * i)? as i16;
                f.coefs.push((c1, c2));
            }
        }
        if tag == WAVE_FORMAT_ADPCM && f.coefs.is_empty() {
            f.coefs = adpcm::MS_COEFS.to_vec();
        }
    }
    Ok(f)
}

fn pcm16_for(src: &WaveFmt) -> WaveFmt {
    WaveFmt {
        tag: WAVE_FORMAT_PCM,
        channels: src.channels,
        rate: src.rate,
        avg_bytes: src.rate * src.channels as u32 * 2,
        block_align: src.channels * 2,
        bits: 16,
        samples_per_block: 0,
        coefs: Vec::new(),
    }
}

fn write_pcm_wfx(s: &mut Session, p: u32, f: &WaveFmt, size: u32) -> Result<(), ApiError> {
    let mut b = Vec::with_capacity(18);
    b.extend(f.tag.to_le_bytes());
    b.extend(f.channels.to_le_bytes());
    b.extend(f.rate.to_le_bytes());
    b.extend(f.avg_bytes.to_le_bytes());
    b.extend(f.block_align.to_le_bytes());
    b.extend(f.bits.to_le_bytes());
    b.extend(0u16.to_le_bytes());
    b.truncate(size as usize);
    s.mem.write(p, &b)?;
    Ok(())
}

/// Whether `src` → `dst` is a conversion the installed codecs perform.
fn supported(src: &WaveFmt, dst: &WaveFmt) -> bool {
    let pcm_out = dst.tag == WAVE_FORMAT_PCM && dst.channels == src.channels && dst.rate == src.rate;
    let header = if src.tag == WAVE_FORMAT_ADPCM { 7 } else { 4 };
    let block_ok = src.block_align as usize >= header * src.channels as usize;
    match src.tag {
        WAVE_FORMAT_ADPCM | WAVE_FORMAT_IMA_ADPCM => {
            pcm_out && dst.bits == 16 && src.bits == 4 && matches!(src.channels, 1 | 2) && block_ok
        }
        WAVE_FORMAT_PCM => pcm_out && dst.bits == src.bits,
        _ => false,
    }
}

fn samples_per_block(f: &WaveFmt) -> usize {
    let (ba, ch) = (f.block_align as usize, f.channels as usize);
    match f.tag {
        WAVE_FORMAT_ADPCM => adpcm::ms_samples_per_block(ba, ch),
        WAVE_FORMAT_IMA_ADPCM => adpcm::ima_samples_per_block(ba, ch),
        _ => 1,
    }
}

/// Output bytes for `n` input bytes (whole blocks plus a decodable partial block).
fn dst_size(st: &AcmStream, n: u32) -> u32 {
    if st.src.tag == WAVE_FORMAT_PCM {
        return n;
    }
    let ba = st.src.block_align as u32;
    let ch = st.src.channels as u32;
    let spb = samples_per_block(&st.src) as u32;
    let full = n / ba;
    let rest = n % ba;
    let mut samples = full * spb;
    let hdr = if st.src.tag == WAVE_FORMAT_ADPCM { 7 } else { 4 } * ch;
    if rest >= hdr {
        samples += if st.src.tag == WAVE_FORMAT_ADPCM {
            (rest - hdr) * 2 / ch + 2
        } else {
            (rest - hdr) / (4 * ch) * 8 + 1
        };
    }
    samples * ch * 2
}

/// Input bytes that produce at most `n` output bytes.
fn src_size(st: &AcmStream, n: u32) -> u32 {
    if st.src.tag == WAVE_FORMAT_PCM {
        return n;
    }
    let per_block = samples_per_block(&st.src) as u32 * st.src.channels as u32 * 2;
    (n / per_block.max(1)) * st.src.block_align as u32
}

fn acm_metrics(s: &mut Session) -> ApiResult {
    let [_obj, metric, p] = s.args::<3>()?;
    let v = match metric {
        1..=3 => 2,            // ACM_METRIC_COUNT_DRIVERS / CODECS / CONVERTERS
        4 => 0,                // COUNT_FILTERS
        5..=10 => 0,           // disabled / hardware / local drivers
        50 => MAX_FORMAT_SIZE, // ACM_METRIC_MAX_SIZE_FORMAT
        51 => 0,               // ACM_METRIC_MAX_SIZE_FILTER
        _ => {
            s.missing("acmMetrics", format!("metric {metric}"));
            return Ok(ApiRet::Ret(MMSYSERR_NOTSUPPORTED));
        }
    };
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(0))
}

fn acm_format_suggest(s: &mut Session) -> ApiResult {
    let [_had, psrc, pdst, cb, flags] = s.args::<5>()?;
    let src = read_wfx(s, psrc)?;
    let mut dst = if src.tag == WAVE_FORMAT_PCM {
        src.clone()
    } else {
        pcm16_for(&src)
    };
    // ACM_FORMATSUGGESTF_*: fields of the destination the caller fixed.
    let fixed = s.mem.read_vec(pdst, 16)?;
    let g16 = |o: usize| u16::from_le_bytes([fixed[o], fixed[o + 1]]);
    if flags & 0x1_0000 != 0 && g16(0) != WAVE_FORMAT_PCM {
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    if flags & 0x2_0000 != 0 && g16(2) != dst.channels {
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    if flags & 0x4_0000 != 0 && u32::from_le_bytes(fixed[4..8].try_into().unwrap()) != dst.rate {
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    if flags & 0x8_0000 != 0 && g16(14) != dst.bits {
        // Sample size conversions are not offered.
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    if !supported(&src, &dst) {
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    dst.avg_bytes = dst.rate * dst.block_align as u32;
    if cb < 16 {
        return Ok(ApiRet::Ret(MMSYSERR_INVALPARAM));
    }
    write_pcm_wfx(s, pdst, &dst, cb.min(18))?;
    Ok(ApiRet::Ret(0))
}

fn acm_stream_open(s: &mut Session) -> ApiResult {
    let [phas, _had, psrc, pdst, pfilter, _cb, _inst, flags] = s.args::<8>()?;
    if pfilter != 0 {
        s.missing("acmStreamOpen", "filters".into());
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    let src = read_wfx(s, psrc)?;
    let dst = read_wfx(s, pdst)?;
    if !supported(&src, &dst) {
        s.event(
            crate::diag::Severity::Info,
            "acmStreamOpen",
            format!(
                "no codec converts format {} ({} ch, {} Hz, {} bit) to {} ({} ch, {} Hz, {} bit)",
                src.tag, src.channels, src.rate, src.bits, dst.tag, dst.channels, dst.rate, dst.bits
            ),
        );
        return Ok(ApiRet::Ret(ACMERR_NOTPOSSIBLE));
    }
    if flags & 0x1 != 0 {
        // ACM_STREAMOPENF_QUERY
        return Ok(ApiRet::Ret(0));
    }
    if flags & 0x2 != 0 {
        s.missing("acmStreamOpen", "asynchronous conversion".into());
        return Ok(ApiRet::Ret(MMSYSERR_INVALFLAG));
    }
    let h = s.winmm.next_acm.max(1);
    s.winmm.next_acm = h + 1;
    let h = 0x0005_0000 | h;
    s.winmm.acm.insert(h, AcmStream { src, dst });
    if phas != 0 {
        s.mem.write_u32(phas, h)?;
    }
    Ok(ApiRet::Ret(0))
}

fn acm_stream_close(s: &mut Session) -> ApiResult {
    let [h, _flags] = s.args::<2>()?;
    Ok(ApiRet::Ret(if s.winmm.acm.remove(&h).is_some() {
        0
    } else {
        MMSYSERR_INVALHANDLE
    }))
}

fn acm_stream_size(s: &mut Session) -> ApiResult {
    let [h, n, pout, flags] = s.args::<4>()?;
    let Some(st) = s.winmm.acm.get(&h).cloned() else {
        return Ok(ApiRet::Ret(MMSYSERR_INVALHANDLE));
    };
    let v = match flags & 0xf {
        0 => dst_size(&st, n),
        1 => src_size(&st, n),
        _ => return Ok(ApiRet::Ret(MMSYSERR_INVALFLAG)),
    };
    s.mem.write_u32(pout, v)?;
    Ok(ApiRet::Ret(0))
}

fn acm_stream_prepare_header(s: &mut Session) -> ApiResult {
    let [h, p, _flags] = s.args::<3>()?;
    if !s.winmm.acm.contains_key(&h) {
        return Ok(ApiRet::Ret(MMSYSERR_INVALHANDLE));
    }
    let st = s.mem.read_u32(p + 4)?;
    s.mem.write_u32(
        p + 4,
        (st | ACMSTREAMHEADER_STATUSF_PREPARED) & !ACMSTREAMHEADER_STATUSF_DONE,
    )?;
    Ok(ApiRet::Ret(0))
}

fn acm_stream_unprepare_header(s: &mut Session) -> ApiResult {
    let [h, p, _flags] = s.args::<3>()?;
    if !s.winmm.acm.contains_key(&h) {
        return Ok(ApiRet::Ret(MMSYSERR_INVALHANDLE));
    }
    let st = s.mem.read_u32(p + 4)?;
    if st & ACMSTREAMHEADER_STATUSF_PREPARED == 0 {
        return Ok(ApiRet::Ret(ACMERR_UNPREPARED));
    }
    s.mem.write_u32(p + 4, st & !ACMSTREAMHEADER_STATUSF_PREPARED)?;
    Ok(ApiRet::Ret(0))
}

fn acm_stream_convert(s: &mut Session) -> ApiResult {
    let [h, p, _flags] = s.args::<3>()?;
    let Some(st) = s.winmm.acm.get(&h).cloned() else {
        return Ok(ApiRet::Ret(MMSYSERR_INVALHANDLE));
    };
    // ACMSTREAMHEADER: cbStruct, fdwStatus, dwUser, pbSrc, cbSrcLength,
    // cbSrcLengthUsed, dwSrcUser, pbDst, cbDstLength, cbDstLengthUsed, ...
    let status = s.mem.read_u32(p + 4)?;
    if status & ACMSTREAMHEADER_STATUSF_PREPARED == 0 {
        return Ok(ApiRet::Ret(ACMERR_UNPREPARED));
    }
    let (psrc, nsrc) = (s.mem.read_u32(p + 12)?, s.mem.read_u32(p + 16)?);
    let (pdst, ndst) = (s.mem.read_u32(p + 28)?, s.mem.read_u32(p + 32)?);
    let src = s.mem.read_vec(psrc, nsrc)?;
    let ch = st.src.channels as usize;
    let mut out: Vec<u8> = Vec::new();
    let mut used = 0usize;
    if st.src.tag == WAVE_FORMAT_PCM {
        let n = (nsrc.min(ndst) as usize / st.src.block_align.max(1) as usize) * st.src.block_align.max(1) as usize;
        out.extend_from_slice(&src[..n]);
        used = n;
    } else {
        let ba = st.src.block_align as usize;
        while used < src.len() {
            let end = (used + ba).min(src.len());
            let block = &src[used..end];
            let pcm = match st.src.tag {
                WAVE_FORMAT_ADPCM => adpcm::ms_decode_block(block, ch, &st.src.coefs),
                _ => adpcm::ima_decode_block(block, ch),
            };
            let Some(pcm) = pcm else { break };
            if out.len() + pcm.len() * 2 > ndst as usize {
                break;
            }
            out.extend(pcm.iter().flat_map(|v| v.to_le_bytes()));
            used = end;
        }
    }
    s.mem.write(pdst, &out)?;
    s.mem.write_u32(p + 20, used as u32)?;
    s.mem.write_u32(p + 36, out.len() as u32)?;
    s.mem.write_u32(p + 4, status | ACMSTREAMHEADER_STATUSF_DONE)?;
    Ok(ApiRet::Ret(0))
}

pub static MSACM32_EXPORTS: &[ExportDef] = &[
    export!("acmFormatSuggest", 5, acm_format_suggest),
    export!("acmMetrics", 3, acm_metrics),
    export!("acmStreamClose", 2, acm_stream_close),
    export!("acmStreamConvert", 3, acm_stream_convert),
    export!("acmStreamOpen", 8, acm_stream_open),
    export!("acmStreamPrepareHeader", 3, acm_stream_prepare_header),
    export!("acmStreamSize", 4, acm_stream_size),
    export!("acmStreamUnprepareHeader", 3, acm_stream_unprepare_header),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn adpcm_fmt(tag: u16, ch: u16, ba: u16) -> WaveFmt {
        WaveFmt {
            tag,
            channels: ch,
            rate: 22050,
            avg_bytes: 0,
            block_align: ba,
            bits: 4,
            samples_per_block: 0,
            coefs: adpcm::MS_COEFS.to_vec(),
        }
    }

    #[test]
    fn stream_sizes() {
        let src = adpcm_fmt(WAVE_FORMAT_ADPCM, 1, 256);
        let st = AcmStream {
            dst: pcm16_for(&src),
            src,
        };
        assert_eq!(dst_size(&st, 512), 2 * 500 * 2);
        assert_eq!(src_size(&st, 2000), 512);
        assert_eq!(src_size(&st, 1999), 256);
        // A partial block of a 7-byte header and 10 data bytes gives 22 samples.
        assert_eq!(dst_size(&st, 256 + 17), (500 + 22) * 2);
        let ima = adpcm_fmt(WAVE_FORMAT_IMA_ADPCM, 1, 256);
        let st = AcmStream {
            dst: pcm16_for(&ima),
            src: ima,
        };
        assert_eq!(dst_size(&st, 256), 505 * 2);
    }
}
