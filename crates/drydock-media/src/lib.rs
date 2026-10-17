//! Media formats for Drydock's Video for Windows support: the AVI container and the
//! Indeo 5 video codec.
//!
//! The Indeo 5 decoder is a port of FFmpeg's (LGPL-2.1-or-later), used here under the
//! GPL-2.0 as that licence allows; see the notices at the top of its source files.

pub mod avi;
mod bits;
pub mod indeo5;
mod ivi_dsp;
mod ivi_tables;
