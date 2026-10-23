//! Joysticks for Drydock: the force-feedback effect model shared by the emulated
//! DirectInput device and the host, HID report descriptor parsing, a USB PID
//! (Physical Interface Device) force-feedback driver, and host game controllers: joysticks
//! through HID and Xbox-style gamepads through gilrs.

#[cfg(feature = "device")]
pub mod device;
pub mod ff;
#[cfg(feature = "gamepad")]
pub mod gamepad;
pub mod hid;
pub mod input;
pub mod pid;
