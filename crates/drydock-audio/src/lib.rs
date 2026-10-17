//! Audio outputs for the virtual sound device.
//!
//! The emulated mixer produces interleaved stereo `f32` frames at [`OUTPUT_RATE`] as guest
//! time advances. Outputs only consume those frames: a host device that underruns plays
//! silence, one that is behind drops old frames. Neither ever feeds back into guest time.

use std::collections::VecDeque;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const OUTPUT_RATE: u32 = 44_100;

pub trait AudioSink {
    /// Interleaved stereo samples in [-1, 1].
    fn push(&mut self, stereo: &[f32]);
    fn name(&self) -> String;
    /// Frames the host has not played yet (for diagnostics only).
    fn queued_frames(&self) -> usize {
        0
    }
    /// Drop buffered host audio (e.g. on pause), keeping the virtual device intact.
    fn flush(&mut self) {}
}

/// Consumes and discards audio.
pub struct NullSink {
    pub frames: u64,
}

impl AudioSink for NullSink {
    fn push(&mut self, stereo: &[f32]) {
        self.frames += stereo.len() as u64 / 2;
    }
    fn name(&self) -> String {
        "discard".into()
    }
}

/// Writes 16-bit PCM WAV.
pub struct WavSink {
    file: std::fs::File,
    frames: u64,
}

impl WavSink {
    pub fn create(path: &Path) -> std::io::Result<WavSink> {
        let mut file = std::fs::File::create(path)?;
        file.write_all(&[0u8; 44])?;
        Ok(WavSink { file, frames: 0 })
    }
    fn finalize(&mut self) -> std::io::Result<()> {
        let data = (self.frames * 4) as u32;
        let mut h = Vec::with_capacity(44);
        h.extend(b"RIFF");
        h.extend((36 + data).to_le_bytes());
        h.extend(b"WAVEfmt ");
        h.extend(16u32.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(2u16.to_le_bytes());
        h.extend(OUTPUT_RATE.to_le_bytes());
        h.extend((OUTPUT_RATE * 4).to_le_bytes());
        h.extend(4u16.to_le_bytes());
        h.extend(16u16.to_le_bytes());
        h.extend(b"data");
        h.extend(data.to_le_bytes());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&h)?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

impl AudioSink for WavSink {
    fn push(&mut self, stereo: &[f32]) {
        let mut b = Vec::with_capacity(stereo.len() * 2);
        for s in stereo {
            b.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        let _ = self.file.write_all(&b);
        self.frames += stereo.len() as u64 / 2;
        if self.frames % (OUTPUT_RATE as u64) < (stereo.len() as u64 / 2) {
            let _ = self.finalize();
        }
    }
    fn name(&self) -> String {
        "wav".into()
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        let _ = self.finalize();
    }
}

/// Fan-out to several sinks (e.g. play and capture).
pub struct Tee(pub Vec<Box<dyn AudioSink>>);

impl AudioSink for Tee {
    fn push(&mut self, stereo: &[f32]) {
        for s in &mut self.0 {
            s.push(stereo);
        }
    }
    fn name(&self) -> String {
        self.0.iter().map(|s| s.name()).collect::<Vec<_>>().join("+")
    }
    fn queued_frames(&self) -> usize {
        self.0.iter().map(|s| s.queued_frames()).max().unwrap_or(0)
    }
    fn flush(&mut self) {
        for s in &mut self.0 {
            s.flush();
        }
    }
}

#[cfg(feature = "device")]
pub use device::DeviceSink;

#[cfg(feature = "device")]
mod device {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// Plays through the default host output device.
    pub struct DeviceSink {
        ring: Arc<Mutex<VecDeque<f32>>>,
        _stream: cpal::Stream,
        cap: usize,
        name: String,
    }

    impl DeviceSink {
        pub fn open() -> Result<DeviceSink, String> {
            let host = cpal::default_host();
            let dev = host.default_output_device().ok_or("no audio output device")?;
            let name = dev.name().unwrap_or_else(|_| "default".into());
            let cfg = cpal::StreamConfig {
                channels: 2,
                sample_rate: cpal::SampleRate(OUTPUT_RATE),
                buffer_size: cpal::BufferSize::Default,
            };
            let ring = Arc::new(Mutex::new(VecDeque::<f32>::new()));
            let r2 = ring.clone();
            let stream = dev
                .build_output_stream(
                    &cfg,
                    move |out: &mut [f32], _| {
                        let mut q = r2.lock().unwrap();
                        for o in out.iter_mut() {
                            *o = q.pop_front().unwrap_or(0.0);
                        }
                    },
                    |e| log::warn!("audio stream error: {e}"),
                    None,
                )
                .map_err(|e| format!("audio stream: {e}"))?;
            stream.play().map_err(|e| format!("audio play: {e}"))?;
            Ok(DeviceSink {
                ring,
                _stream: stream,
                // At most 200 ms queued; older audio is dropped if the host falls behind.
                cap: (OUTPUT_RATE as usize / 5) * 2,
                name,
            })
        }
    }

    impl AudioSink for DeviceSink {
        fn push(&mut self, stereo: &[f32]) {
            let mut q = self.ring.lock().unwrap();
            q.extend(stereo.iter().copied());
            while q.len() > self.cap {
                q.pop_front();
            }
        }
        fn name(&self) -> String {
            format!("device:{}", self.name)
        }
        fn queued_frames(&self) -> usize {
            self.ring.lock().unwrap().len() / 2
        }
        fn flush(&mut self) {
            self.ring.lock().unwrap().clear();
        }
    }
}
