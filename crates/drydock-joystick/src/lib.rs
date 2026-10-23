//! Joysticks for Drydock: the force-feedback effect model shared by the emulated
//! DirectInput device and the host, HID report descriptor parsing, a USB PID
//! (Physical Interface Device) force-feedback driver, and host game controllers.

#[cfg(feature = "device")]
pub mod device;
pub mod ff;
pub mod hid;
pub mod input;
pub mod pid;
