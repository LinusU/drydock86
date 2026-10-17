//! Drydock Win32/COM environment.
//!
//! A [`session::Session`] is one emulated 32-bit Windows process. All guest-visible
//! state (memory, threads, kernel objects, windows, devices) is owned by the session
//! with stable internal ids, so it can be inspected, driven and checkpointed.

pub mod api;
pub mod com;
pub mod diag;
pub mod dll;
pub mod heap;
pub mod input;
pub mod loader;
pub mod objects;
pub mod observe;
pub mod reg;
pub mod sched;
pub mod seh;
pub mod session;
pub mod strings;
pub mod surface;
pub mod sync;
pub mod thunks;
pub mod vfs;
