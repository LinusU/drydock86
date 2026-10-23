//! Virtual input devices shared by window messages, GetAsyncKeyState and DirectInput.
//!
//! Human input (from a window) and scripted input (scenarios, protocol) both enter here
//! as timestamped events. Events are applied when virtual time reaches their timestamp,
//! updating device state and fanning out to the consumers.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    /// Virtual-key code, PC/AT set-1 scan code (0xE0-prefixed codes have `extended`).
    Key {
        vk: u8,
        scan: u16,
        #[serde(default)]
        extended: bool,
        down: bool,
    },
    /// Absolute pointer position in screen coordinates.
    MouseMove {
        x: i32,
        y: i32,
    },
    /// Relative motion (mickeys), e.g. from a captured host pointer.
    MouseRel {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: u8,
        down: bool,
    },
    Wheel {
        delta: i32,
    },
    /// Joystick axes in the range 0..=65535: x, y, z (throttle), r (twist/rudder).
    JoyAxes {
        x: u16,
        y: u16,
        z: u16,
        r: u16,
    },
    JoyButton {
        button: u8,
        down: bool,
    },
    /// Point-of-view hat in hundredths of a degree clockwise from north, or
    /// 4294967295 (0xFFFFFFFF) when centred.
    JoyPov {
        pov: u32,
    },
    /// The emulated window gained/lost host focus.
    Focus {
        active: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimedInput {
    pub at_ns: u64,
    pub ev: InputEvent,
}

pub const POV_CENTERED: u32 = u32::MAX;

fn pov_centered() -> u32 {
    POV_CENTERED
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputState {
    pub keys: Vec<bool>,
    /// GetAsyncKeyState "pressed since last call" bits.
    pub async_pressed: Vec<bool>,
    pub toggles: Vec<bool>,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub buttons: u8,
    pub joy_present: bool,
    pub joy_axes: [u16; 4],
    pub joy_buttons: u32,
    #[serde(default = "pov_centered")]
    pub joy_pov: u32,
    pub pending: VecDeque<TimedInput>,
    /// Applied events (with time), consumed by DirectInput devices.
    pub log: Vec<(u64, InputEvent)>,
    pub log_base: usize,
    /// Every event ever applied, for recording input timelines.
    pub recorded: Vec<TimedInput>,
    pub host_focus: bool,
}

impl Default for InputState {
    fn default() -> Self {
        InputState {
            keys: vec![false; 256],
            async_pressed: vec![false; 256],
            toggles: vec![false; 256],
            mouse_x: 320,
            mouse_y: 240,
            buttons: 0,
            joy_present: false,
            joy_axes: [32768; 4],
            joy_buttons: 0,
            joy_pov: POV_CENTERED,
            pending: VecDeque::new(),
            log: Vec::new(),
            log_base: 0,
            recorded: Vec::new(),
            host_focus: true,
        }
    }
}

impl InputState {
    /// Schedule an event; events are kept sorted by time (stable for equal times).
    pub fn schedule(&mut self, at_ns: u64, ev: InputEvent) {
        let idx = self
            .pending
            .iter()
            .position(|e| e.at_ns > at_ns)
            .unwrap_or(self.pending.len());
        self.pending.insert(idx, TimedInput { at_ns, ev });
    }

    pub fn next_due(&self) -> Option<u64> {
        self.pending.front().map(|e| e.at_ns)
    }

    /// Release every pressed key and button (used when an experiment is cancelled).
    pub fn release_all(&mut self, now: u64) {
        for vk in 0..256usize {
            if self.keys[vk] {
                self.schedule(
                    now,
                    InputEvent::Key {
                        vk: vk as u8,
                        scan: vk_to_scan(vk as u8),
                        extended: false,
                        down: false,
                    },
                );
            }
        }
        for b in 0..5 {
            if self.buttons & (1 << b) != 0 {
                self.schedule(now, InputEvent::MouseButton { button: b, down: false });
            }
        }
    }

    pub fn apply_state(&mut self, ev: &InputEvent) {
        match *ev {
            InputEvent::Key { vk, down, .. } => {
                let v = vk as usize;
                if down && !self.keys[v] {
                    self.async_pressed[v] = true;
                    self.toggles[v] = !self.toggles[v];
                }
                self.keys[v] = down;
                // Generic modifier keys mirror their left/right variants.
                let generic = match vk {
                    0xa0 | 0xa1 => Some(0x10),
                    0xa2 | 0xa3 => Some(0x11),
                    0xa4 | 0xa5 => Some(0x12),
                    _ => None,
                };
                if let Some(g) = generic {
                    let (l, r) = match g {
                        0x10 => (0xa0, 0xa1),
                        0x11 => (0xa2, 0xa3),
                        _ => (0xa4, 0xa5),
                    };
                    let any = self.keys[l] || self.keys[r];
                    if any && !self.keys[g] {
                        self.async_pressed[g] = true;
                    }
                    self.keys[g] = any;
                }
            }
            InputEvent::MouseMove { x, y } => {
                self.mouse_x = x;
                self.mouse_y = y;
            }
            InputEvent::MouseRel { dx, dy } => {
                self.mouse_x += dx;
                self.mouse_y += dy;
            }
            InputEvent::MouseButton { button, down } => {
                let vk = [0x01u8, 0x02, 0x04, 0x05, 0x06][button.min(4) as usize] as usize;
                if down {
                    if self.buttons & (1 << button) == 0 {
                        self.async_pressed[vk] = true;
                    }
                    self.buttons |= 1 << button;
                } else {
                    self.buttons &= !(1 << button);
                }
                self.keys[vk] = down;
            }
            InputEvent::JoyAxes { x, y, z, r } => self.joy_axes = [x, y, z, r],
            InputEvent::JoyButton { button, down } => {
                if down {
                    self.joy_buttons |= 1 << button;
                } else {
                    self.joy_buttons &= !(1 << button);
                }
            }
            InputEvent::JoyPov { pov } => self.joy_pov = pov,
            InputEvent::Wheel { .. } => {}
            InputEvent::Focus { active } => self.host_focus = active,
        }
    }
}

/// US keyboard layout: VK → set-1 scan code (non-extended part).
pub fn vk_to_scan(vk: u8) -> u16 {
    match vk {
        0x08 => 0x0e,
        0x09 => 0x0f,
        0x0d => 0x1c,
        0x10 | 0xa0 => 0x2a,
        0xa1 => 0x36,
        0x11 | 0xa2 | 0xa3 => 0x1d,
        0x12 | 0xa4 | 0xa5 => 0x38,
        0x13 => 0x45,
        0x14 => 0x3a,
        0x1b => 0x01,
        0x20 => 0x39,
        0x21 => 0x49,
        0x22 => 0x51,
        0x23 => 0x4f,
        0x24 => 0x47,
        0x25 => 0x4b,
        0x26 => 0x48,
        0x27 => 0x4d,
        0x28 => 0x50,
        0x2c => 0x37,
        0x2d => 0x52,
        0x2e => 0x53,
        b'0' => 0x0b,
        b'1'..=b'9' => 0x02 + (vk - b'1') as u16,
        b'A'..=b'Z' => [
            0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
            0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
        ][(vk - b'A') as usize],
        0x60 => 0x52,
        0x61 => 0x4f,
        0x62 => 0x50,
        0x63 => 0x51,
        0x64 => 0x4b,
        0x65 => 0x4c,
        0x66 => 0x4d,
        0x67 => 0x47,
        0x68 => 0x48,
        0x69 => 0x49,
        0x6a => 0x37,
        0x6b => 0x4e,
        0x6d => 0x4a,
        0x6e => 0x53,
        0x6f => 0x35,
        0x70..=0x79 => 0x3b + (vk - 0x70) as u16,
        0x7a => 0x57,
        0x7b => 0x58,
        0x90 => 0x45,
        0x91 => 0x46,
        0xba => 0x27,
        0xbb => 0x0d,
        0xbc => 0x33,
        0xbd => 0x0c,
        0xbe => 0x34,
        0xbf => 0x35,
        0xc0 => 0x29,
        0xdb => 0x1a,
        0xdc => 0x2b,
        0xdd => 0x1b,
        0xde => 0x28,
        _ => 0,
    }
}

/// Keys that use the 0xE0 extended prefix on a PC keyboard.
pub fn vk_is_extended(vk: u8) -> bool {
    matches!(
        vk,
        0x21..=0x28 | 0x2d | 0x2e | 0x5b | 0x5c | 0x5d | 0x6f | 0xa3 | 0xa5 | 0x90 | 0x2c
    )
}

/// Character produced by a key on a US layout (for WM_CHAR), honouring shift/caps.
pub fn vk_to_char(vk: u8, shift: bool, caps: bool) -> Option<u8> {
    let c = match vk {
        b'A'..=b'Z' => {
            let upper = shift ^ caps;
            if upper { vk } else { vk + 32 }
        }
        b'0'..=b'9' => {
            if shift {
                b")!@#$%^&*("[(vk - b'0') as usize]
            } else {
                vk
            }
        }
        0x20 => b' ',
        0x0d => b'\r',
        0x08 => 0x08,
        0x09 => b'\t',
        0x1b => 0x1b,
        0xba => {
            if shift {
                b':'
            } else {
                b';'
            }
        }
        0xbb => {
            if shift {
                b'+'
            } else {
                b'='
            }
        }
        0xbc => {
            if shift {
                b'<'
            } else {
                b','
            }
        }
        0xbd => {
            if shift {
                b'_'
            } else {
                b'-'
            }
        }
        0xbe => {
            if shift {
                b'>'
            } else {
                b'.'
            }
        }
        0xbf => {
            if shift {
                b'?'
            } else {
                b'/'
            }
        }
        0xc0 => {
            if shift {
                b'~'
            } else {
                b'`'
            }
        }
        0xdb => {
            if shift {
                b'{'
            } else {
                b'['
            }
        }
        0xdc => {
            if shift {
                b'|'
            } else {
                b'\\'
            }
        }
        0xdd => {
            if shift {
                b'}'
            } else {
                b']'
            }
        }
        0xde => {
            if shift {
                b'"'
            } else {
                b'\''
            }
        }
        0x60..=0x69 => b'0' + (vk - 0x60),
        0x6a => b'*',
        0x6b => b'+',
        0x6d => b'-',
        0x6e => b'.',
        0x6f => b'/',
        _ => return None,
    };
    Some(c)
}

/// Key names as reported by GetKeyNameText for set-1 scan codes (US layout).
pub fn scan_name(scan: u16, extended: bool) -> Option<&'static str> {
    if extended {
        return Some(match scan {
            0x1c => "Num Enter",
            0x1d => "Right Ctrl",
            0x35 => "Num /",
            0x38 => "Right Alt",
            0x47 => "Home",
            0x48 => "Up",
            0x49 => "Page Up",
            0x4b => "Left",
            0x4d => "Right",
            0x4f => "End",
            0x50 => "Down",
            0x51 => "Page Down",
            0x52 => "Insert",
            0x53 => "Delete",
            0x5b => "Left Windows",
            0x5c => "Right Windows",
            0x5d => "Application",
            _ => return None,
        });
    }
    Some(match scan {
        0x01 => "Esc",
        0x02 => "1",
        0x03 => "2",
        0x04 => "3",
        0x05 => "4",
        0x06 => "5",
        0x07 => "6",
        0x08 => "7",
        0x09 => "8",
        0x0a => "9",
        0x0b => "0",
        0x0c => "-",
        0x0d => "=",
        0x0e => "Backspace",
        0x0f => "Tab",
        0x10 => "Q",
        0x11 => "W",
        0x12 => "E",
        0x13 => "R",
        0x14 => "T",
        0x15 => "Y",
        0x16 => "U",
        0x17 => "I",
        0x18 => "O",
        0x19 => "P",
        0x1a => "[",
        0x1b => "]",
        0x1c => "Enter",
        0x1d => "Ctrl",
        0x1e => "A",
        0x1f => "S",
        0x20 => "D",
        0x21 => "F",
        0x22 => "G",
        0x23 => "H",
        0x24 => "J",
        0x25 => "K",
        0x26 => "L",
        0x27 => ";",
        0x28 => "'",
        0x29 => "`",
        0x2a => "Shift",
        0x2b => "\\",
        0x2c => "Z",
        0x2d => "X",
        0x2e => "C",
        0x2f => "V",
        0x30 => "B",
        0x31 => "N",
        0x32 => "M",
        0x33 => ",",
        0x34 => ".",
        0x35 => "/",
        0x36 => "Right Shift",
        0x37 => "Num *",
        0x38 => "Alt",
        0x39 => "Space",
        0x3a => "Caps Lock",
        0x3b => "F1",
        0x3c => "F2",
        0x3d => "F3",
        0x3e => "F4",
        0x3f => "F5",
        0x40 => "F6",
        0x41 => "F7",
        0x42 => "F8",
        0x43 => "F9",
        0x44 => "F10",
        0x45 => "Pause",
        0x46 => "Scroll Lock",
        0x47 => "Num 7",
        0x48 => "Num 8",
        0x49 => "Num 9",
        0x4a => "Num -",
        0x4b => "Num 4",
        0x4c => "Num 5",
        0x4d => "Num 6",
        0x4e => "Num +",
        0x4f => "Num 1",
        0x50 => "Num 2",
        0x51 => "Num 3",
        0x52 => "Num 0",
        0x53 => "Num Del",
        0x57 => "F11",
        0x58 => "F12",
        _ => return None,
    })
}

/// Parse a key name used in scenarios ("A", "ENTER", "LEFT", "F1", "0x41", …) to a VK.
pub fn parse_vk(name: &str) -> Option<u8> {
    let n = name.trim().to_uppercase();
    if let Some(h) = n.strip_prefix("0X") {
        return u8::from_str_radix(h, 16).ok();
    }
    if n.len() == 1 {
        let c = n.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some(c);
        }
    }
    if let Some(f) = n.strip_prefix('F')
        && let Ok(k) = f.parse::<u8>()
        && (1..=24).contains(&k)
    {
        return Some(0x6f + k);
    }
    Some(match n.as_str() {
        "BACKSPACE" | "BACK" => 0x08,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0d,
        "SHIFT" => 0x10,
        "CTRL" | "CONTROL" => 0x11,
        "ALT" | "MENU" => 0x12,
        "PAUSE" => 0x13,
        "CAPSLOCK" => 0x14,
        "ESC" | "ESCAPE" => 0x1b,
        "SPACE" => 0x20,
        "PAGEUP" | "PRIOR" => 0x21,
        "PAGEDOWN" | "NEXT" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "INSERT" => 0x2d,
        "DELETE" => 0x2e,
        "LSHIFT" => 0xa0,
        "RSHIFT" => 0xa1,
        "LCTRL" => 0xa2,
        "RCTRL" => 0xa3,
        "LALT" => 0xa4,
        "RALT" => 0xa5,
        "NUMPAD0" => 0x60,
        "NUMPAD1" => 0x61,
        "NUMPAD2" => 0x62,
        "NUMPAD3" => 0x63,
        "NUMPAD4" => 0x64,
        "NUMPAD5" => 0x65,
        "NUMPAD6" => 0x66,
        "NUMPAD7" => 0x67,
        "NUMPAD8" => 0x68,
        "NUMPAD9" => 0x69,
        _ => return None,
    })
}
