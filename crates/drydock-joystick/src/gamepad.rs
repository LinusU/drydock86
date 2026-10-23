//! Host gamepads (Xbox, PlayStation and Switch style) as the emulated joystick, through
//! gilrs. Controllers without an SDL gamepad mapping are real joysticks and go through
//! [`crate::device`] instead.
//!
//! Left stick is X/Y, the right stick's horizontal axis is the rudder (R), and the
//! triggers form a throttle on Z (right trigger pushes it towards 0, left trigger towards
//! 65535, matching a stick's "forward is low" throttle). The D-pad is the hat and also
//! buttons 11 to 14, for games that bind it as buttons.

use crate::device::Event;
use crate::input::{JoyState, POV_CENTERED};
use gilrs::{Axis, Button, EventType, Gamepad, GamepadId, Gilrs, MappingSource};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

const DEADZONE: f32 = 0.12;

/// A gamepad the host offers.
#[derive(Debug, Clone)]
pub struct Pad {
    pub name: String,
    pub vid: u16,
    pub pid: u16,
}

impl Pad {
    /// Matches "045e:028e" or part of the name (case-insensitive).
    pub fn matches(&self, filter: &str) -> bool {
        let f = filter.to_lowercase();
        f == format!("{:04x}:{:04x}", self.vid, self.pid) || self.name.to_lowercase().contains(&f)
    }
}

impl std::fmt::Display for Pad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:04x}:{:04x})", self.name, self.vid, self.pid)
    }
}

fn pad_of(g: &Gamepad) -> Pad {
    Pad {
        name: g.name().to_string(),
        vid: g.vendor_id().unwrap_or(0),
        pid: g.product_id().unwrap_or(0),
    }
}

/// Only controllers with a gamepad mapping: the layout is known, unlike a joystick's.
fn is_gamepad(g: &Gamepad) -> bool {
    g.is_connected() && g.mapping_source() == MappingSource::SdlMappings
}

/// The gamepads connected now. Empty when the controller subsystem cannot start.
pub fn connected() -> Vec<Pad> {
    match Gilrs::new() {
        Ok(g) => g.gamepads().filter(|(_, p)| is_gamepad(p)).map(|(_, p)| pad_of(&p)).collect(),
        Err(e) => {
            log::warn!("game controller support unavailable: {e}");
            Vec::new()
        }
    }
}

/// Joystick button numbers: face buttons, shoulders, Back/Start, stick clicks, D-pad.
const BUTTONS: [Button; 14] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::LeftTrigger,
    Button::RightTrigger,
    Button::Select,
    Button::Start,
    Button::LeftThumb,
    Button::RightThumb,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
];

/// Maps -1.0..=1.0 to 0..=65535 with a centre dead zone.
fn axis_value(v: f32) -> u16 {
    let v = if v.abs() < DEADZONE {
        0.0
    } else {
        // Rescale so the output starts from zero at the edge of the dead zone.
        v.signum() * (v.abs() - DEADZONE) / (1.0 - DEADZONE)
    };
    ((v.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16
}

fn axes(x: f32, y: f32, rt: f32, lt: f32, rx: f32) -> [u16; 4] {
    [
        axis_value(x),
        // Controllers report up as positive; joystick Y grows downwards.
        axis_value(-y),
        axis_value(lt - rt),
        axis_value(rx),
    ]
}

/// Hundredths of a degree clockwise from north, or [`POV_CENTERED`].
fn pov(up: bool, down: bool, left: bool, right: bool) -> u32 {
    let (v, h) = (i32::from(down) - i32::from(up), i32::from(right) - i32::from(left));
    match (v, h) {
        (-1, 0) => 0,
        (-1, 1) => 4500,
        (0, 1) => 9000,
        (1, 1) => 13500,
        (1, 0) => 18000,
        (1, -1) => 22500,
        (0, -1) => 27000,
        (-1, -1) => 31500,
        _ => POV_CENTERED,
    }
}

fn state(g: &Gamepad) -> JoyState {
    let trigger = |b| g.button_data(b).map_or(0.0, |d| d.value());
    let mut buttons = 0u32;
    for (n, b) in BUTTONS.iter().enumerate() {
        if g.is_pressed(*b) {
            buttons |= 1 << n;
        }
    }
    JoyState {
        axes: axes(
            g.value(Axis::LeftStickX),
            g.value(Axis::LeftStickY),
            trigger(Button::RightTrigger2),
            trigger(Button::LeftTrigger2),
            g.value(Axis::RightStickX),
        ),
        buttons,
        pov: pov(
            g.is_pressed(Button::DPadUp),
            g.is_pressed(Button::DPadDown),
            g.is_pressed(Button::DPadLeft),
            g.is_pressed(Button::DPadRight),
        ),
    }
}

/// A gamepad thread. Dropping it stops the thread.
pub struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Follow the gamepad that was used last (matching `filter`, when given) and report its
/// state. `events` returns false when nobody is listening any more.
pub fn spawn(filter: Option<String>, events: Box<dyn Fn(Event) -> bool + Send>) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("drydock-gamepad".into())
        .spawn(move || run(filter, events, flag))
        .ok();
    Running { stop, thread }
}

fn run(filter: Option<String>, events: Box<dyn Fn(Event) -> bool + Send>, stop: Arc<AtomicBool>) {
    let mut gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(e) => {
            events(Event::Status(format!("gamepad: unavailable: {e}")));
            return;
        }
    };
    let wanted = |g: &Gamepad| is_gamepad(g) && filter.as_deref().is_none_or(|f| pad_of(g).matches(f));
    let mut active: Option<GamepadId> = None;
    let mut last = JoyState::default();
    let mut waiting_reported = false;
    // Adopt a gamepad that is already connected.
    let first = gilrs.gamepads().find(|(_, g)| wanted(g)).map(|(id, _)| id);
    if let Some(id) = first {
        active = Some(id);
        let g = gilrs.gamepad(id);
        last = state(&g);
        if !events(Event::Status(format!("gamepad: {}", pad_of(&g)))) || !events(Event::State(last)) {
            return;
        }
    }
    while !stop.load(Ordering::Relaxed) {
        if active.is_none() && !waiting_reported {
            let what = filter.as_deref().unwrap_or("a gamepad");
            events(Event::Status(format!("gamepad: waiting for {what} to be connected")));
            waiting_reported = true;
        }
        let Some(ev) = gilrs.next_event_blocking(Some(Duration::from_millis(20))) else {
            continue;
        };
        match ev.event {
            EventType::Disconnected if active == Some(ev.id) => {
                active = None;
                waiting_reported = false;
                last = JoyState::default();
                if !events(Event::Status("gamepad: disconnected".into())) || !events(Event::State(last)) {
                    return;
                }
                continue;
            }
            EventType::Connected | EventType::Dropped | EventType::Disconnected => continue,
            _ => {}
        }
        let g = gilrs.gamepad(ev.id);
        if !wanted(&g) {
            continue;
        }
        if active != Some(ev.id) {
            active = Some(ev.id);
            waiting_reported = false;
            if !events(Event::Status(format!("gamepad: {}", pad_of(&g)))) {
                return;
            }
        }
        let st = state(&g);
        if st != last {
            last = st;
            if !events(Event::State(st)) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_zone_centres_and_extremes_reach_the_ends() {
        assert_eq!(axis_value(0.0), 32767);
        assert_eq!(axis_value(0.05), 32767);
        assert_eq!(axis_value(-1.0), 0);
        assert_eq!(axis_value(1.0), 65535);
    }

    #[test]
    fn stick_up_is_a_low_y_and_right_trigger_a_low_throttle() {
        let [_, y, z, _] = axes(0.0, 1.0, 1.0, 0.0, 0.0);
        assert_eq!(y, 0);
        assert_eq!(z, 0);
    }

    #[test]
    fn dpad_directions_make_a_hat_angle() {
        assert_eq!(pov(false, false, false, false), POV_CENTERED);
        assert_eq!(pov(true, false, false, false), 0);
        assert_eq!(pov(false, true, false, true), 13500);
        assert_eq!(pov(true, false, true, false), 31500);
        assert_eq!(pov(true, true, false, false), POV_CENTERED);
    }
}
