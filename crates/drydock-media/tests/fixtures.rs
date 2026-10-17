//! Decoding self-authored Indeo 5 streams and AVI files.

use drydock_fixtures::media::{Pcm, avi, indeo5_flat_frame};
use drydock_media::avi::{Avi, Kind};
use drydock_media::indeo5::Decoder;

#[test]
fn decodes_flat_intra_frames() {
    let mut dec = Decoder::new(32, 16).unwrap();
    for (i, colour) in [(16u8, 128u8, 128u8), (235, 90, 200), (0, 255, 0)]
        .into_iter()
        .enumerate()
    {
        let f = dec
            .decode(&indeo5_flat_frame(32, 16, i as u32, colour))
            .unwrap()
            .unwrap();
        assert_eq!((f.width, f.height, f.chroma_width, f.chroma_height), (32, 16, 8, 4));
        assert!(f.y.iter().all(|&v| v == colour.0), "luma of frame {i}");
        assert!(f.u.iter().all(|&v| v == colour.1), "u of frame {i}");
        assert!(f.v.iter().all(|&v| v == colour.2), "v of frame {i}");
    }
}

#[test]
fn rejects_frames_before_a_key_frame() {
    let mut dec = Decoder::new(32, 16).unwrap();
    // A null frame (type 4) with no preceding intra frame.
    let null = [0x1f | 4 << 5, 0];
    assert!(dec.decode(&null).is_err());
}

#[test]
fn parses_generated_avi() {
    let frames: Vec<_> = (0..3).map(|i| indeo5_flat_frame(32, 16, i, (100, 128, 128))).collect();
    let samples: Vec<u8> = (0..300).map(|i| (i % 256) as u8).collect();
    let file = avi(
        32,
        16,
        15,
        &frames,
        Some(Pcm {
            rate: 11025,
            samples: &samples,
        }),
    );
    let a = Avi::parse(&file).unwrap();
    assert_eq!(a.streams.len(), 2);
    let v = &a.streams[0];
    assert_eq!(v.kind, Kind::Video);
    assert_eq!(v.chunks.len(), 3);
    assert_eq!(
        &file[v.chunks[1].offset..v.chunks[1].offset + v.chunks[1].size],
        &frames[1][..]
    );
    let s = &a.streams[1];
    assert_eq!(s.wave_format().unwrap().samples_per_sec, 11025);
    let pcm: Vec<u8> = s
        .chunks
        .iter()
        .flat_map(|c| file[c.offset..c.offset + c.size].to_vec())
        .collect();
    assert_eq!(pcm, samples);
}
