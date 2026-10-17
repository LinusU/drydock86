//! Decodes the Indeo 5 video of an AVI file and writes selected frames as PNGs.
//!
//! Usage: avi_frames <file.avi> <out-dir> [frame numbers...]

use drydock_media::avi::{Avi, Kind};
use drydock_media::indeo5::Decoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read AVI");
    let out = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(out).unwrap();
    let wanted: Vec<usize> = args[3..].iter().map(|a| a.parse().unwrap()).collect();
    let avi = Avi::parse(&data).expect("parse AVI");
    let video = avi
        .streams
        .iter()
        .find(|s| s.kind == Kind::Video)
        .expect("video stream");
    let bi = video.bitmap_info().unwrap();
    println!("{:?} {} chunks, {:?}", bi, video.chunks.len(), avi.header);
    let mut dec = Decoder::new(bi.width as u16, bi.height.unsigned_abs() as u16).unwrap();
    let start = std::time::Instant::now();
    let mut last = None;
    for (i, c) in video.chunks.iter().enumerate() {
        match dec.decode(&data[c.offset..c.offset + c.size]) {
            Ok(Some(f)) => last = Some(f),
            Ok(None) => {}
            Err(e) => println!("frame {i}: {e}"),
        }
        if wanted.contains(&i)
            && let Some(f) = &last
        {
            let file = std::fs::File::create(out.join(format!("frame{i:03}.png"))).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), f.width as u32, f.height as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.write_header().unwrap().write_image_data(&f.to_rgb24()).unwrap();
        }
    }
    println!("decoded {} frames in {:?}", video.chunks.len(), start.elapsed());
}
