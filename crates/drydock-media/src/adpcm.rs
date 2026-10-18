//! Microsoft ADPCM (WAVE_FORMAT_ADPCM, tag 2) and IMA/DVI ADPCM (tag 0x11) block
//! decoders producing 16-bit PCM, as the Windows ACM codecs do.

/// Default MS ADPCM coefficient pairs (WAVEFORMATEX extensions normally repeat them).
pub const MS_COEFS: [(i16, i16); 7] = [
    (256, 0),
    (512, -256),
    (0, 0),
    (192, 64),
    (240, 0),
    (460, -208),
    (392, -232),
];

const MS_ADAPT: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

/// Samples per channel in an MS ADPCM block of `block_align` bytes.
pub fn ms_samples_per_block(block_align: usize, channels: usize) -> usize {
    if block_align < 7 * channels {
        return 0;
    }
    (block_align - 7 * channels) * 2 / channels + 2
}

/// Decode one MS ADPCM block (possibly shorter than a full block at the end of the
/// data) into interleaved samples. Returns None if the header is truncated or names a
/// coefficient set that does not exist.
pub fn ms_decode_block(block: &[u8], channels: usize, coefs: &[(i16, i16)]) -> Option<Vec<i16>> {
    if channels == 0 || channels > 2 || block.len() < 7 * channels {
        return None;
    }
    let mut pred = [0usize; 2];
    let mut delta = [0i32; 2];
    let mut s1 = [0i32; 2];
    let mut s2 = [0i32; 2];
    let rd16 = |o: usize| i16::from_le_bytes([block[o], block[o + 1]]) as i32;
    let mut o = 0;
    for p in pred.iter_mut().take(channels) {
        *p = block[o] as usize;
        if *p >= coefs.len() {
            return None;
        }
        o += 1;
    }
    for d in delta.iter_mut().take(channels) {
        *d = rd16(o);
        o += 2;
    }
    for v in s1.iter_mut().take(channels) {
        *v = rd16(o);
        o += 2;
    }
    for v in s2.iter_mut().take(channels) {
        *v = rd16(o);
        o += 2;
    }
    let mut out = Vec::with_capacity((block.len() - o) * 2 + 2 * channels);
    out.extend(s2[..channels].iter().map(|&v| v as i16));
    out.extend(s1[..channels].iter().map(|&v| v as i16));
    let mut ch = 0;
    for &b in &block[o..] {
        for nib in [b >> 4, b & 15] {
            let (c1, c2) = coefs[pred[ch]];
            let signed = ((nib as i32) << 28) >> 28;
            let predicted = (s1[ch] * c1 as i32 + s2[ch] * c2 as i32) >> 8;
            let v = (predicted + signed * delta[ch]).clamp(-32768, 32767);
            out.push(v as i16);
            s2[ch] = s1[ch];
            s1[ch] = v;
            delta[ch] = ((MS_ADAPT[nib as usize] * delta[ch]) >> 8).max(16);
            ch = (ch + 1) % channels;
        }
    }
    Some(out)
}

const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// Samples per channel in an IMA ADPCM block of `block_align` bytes.
pub fn ima_samples_per_block(block_align: usize, channels: usize) -> usize {
    if block_align < 4 * channels {
        return 0;
    }
    (block_align - 4 * channels) * 2 / channels + 1
}

/// Decode one IMA ADPCM block into interleaved samples (4-byte header per channel,
/// then groups of 4 bytes per channel, low nibble first).
pub fn ima_decode_block(block: &[u8], channels: usize) -> Option<Vec<i16>> {
    if channels == 0 || channels > 2 || block.len() < 4 * channels {
        return None;
    }
    let mut pred = [0i32; 2];
    let mut idx = [0i32; 2];
    for c in 0..channels {
        pred[c] = i16::from_le_bytes([block[4 * c], block[4 * c + 1]]) as i32;
        idx[c] = (block[4 * c + 2] as i32).clamp(0, 88);
    }
    let body = &block[4 * channels..];
    let groups = body.len() / (4 * channels);
    let per_ch = 1 + groups * 8;
    let mut out = vec![0i16; per_ch * channels];
    for c in 0..channels {
        out[c] = pred[c] as i16;
    }
    for g in 0..groups {
        for c in 0..channels {
            let chunk = &body[(g * channels + c) * 4..(g * channels + c) * 4 + 4];
            for (k, &b) in chunk.iter().enumerate() {
                for (h, nib) in [b & 15, b >> 4].into_iter().enumerate() {
                    let step = IMA_STEPS[idx[c] as usize];
                    let mut diff = step >> 3;
                    if nib & 4 != 0 {
                        diff += step;
                    }
                    if nib & 2 != 0 {
                        diff += step >> 1;
                    }
                    if nib & 1 != 0 {
                        diff += step >> 2;
                    }
                    pred[c] = if nib & 8 != 0 { pred[c] - diff } else { pred[c] + diff }.clamp(-32768, 32767);
                    idx[c] = (idx[c] + IMA_INDEX[nib as usize]).clamp(0, 88);
                    let i = 1 + g * 8 + k * 2 + h;
                    out[i * channels + c] = pred[c] as i16;
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_header_samples_come_first() {
        // Mono block: predictor 0, delta 16, sample1 100, sample2 50, then two zero
        // nibbles, which repeat the prediction 2*s1 - ... with coefficient (256, 0).
        let mut b = vec![0u8];
        b.extend(16i16.to_le_bytes());
        b.extend(100i16.to_le_bytes());
        b.extend(50i16.to_le_bytes());
        b.push(0x00);
        let out = ms_decode_block(&b, 1, &MS_COEFS).unwrap();
        assert_eq!(out, vec![50, 100, 100, 100]);
        assert_eq!(ms_samples_per_block(256, 1), 500);
    }

    #[test]
    fn ms_nibbles_follow_the_delta() {
        let mut b = vec![0u8];
        b.extend(16i16.to_le_bytes());
        b.extend(0i16.to_le_bytes());
        b.extend(0i16.to_le_bytes());
        b.push(0x70); // +7 * 16, then 0
        let out = ms_decode_block(&b, 1, &MS_COEFS).unwrap();
        assert_eq!(out[2], 112);
        // The next prediction is s1 (coefficient 256) with the adapted delta.
        assert_eq!(out[3], 112);
    }

    #[test]
    fn ima_block_layout() {
        // Mono: header sample 1000, index 0, then 4 bytes = 8 samples.
        let mut b = vec![];
        b.extend(1000i16.to_le_bytes());
        b.extend([0, 0]);
        b.extend([0x07, 0, 0, 0]);
        let out = ima_decode_block(&b, 1).unwrap();
        assert_eq!(out.len(), 9);
        assert_eq!(out[0], 1000);
        // Nibble 7 at step 7: diff = 0 + 7 + 3 + 1 = 11.
        assert_eq!(out[1], 1011);
        assert_eq!(ima_samples_per_block(256, 1), 505);
    }
}
