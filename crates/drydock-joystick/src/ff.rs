//! Force-feedback effects as the emulated DirectInput device describes them.
//!
//! Units follow DirectInput: magnitudes, gains and saturations in -10000..=10000,
//! times in microseconds, angles in hundredths of a degree. The emulated device keeps
//! the guest-visible effect state on virtual time; a [`ForceSink`] only receives the
//! resulting commands, so actuators never feed back into the guest.

use serde::{Deserialize, Serialize};

/// Duration or iteration count that never ends.
pub const INFINITE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Waveform {
    Square,
    Sine,
    Triangle,
    SawtoothUp,
    SawtoothDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionKind {
    Spring,
    Damper,
    Inertia,
    Friction,
}

/// One axis of a condition effect (DICONDITION).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Condition {
    pub offset: i32,
    pub positive_coefficient: i32,
    pub negative_coefficient: i32,
    pub positive_saturation: u32,
    pub negative_saturation: u32,
    pub dead_band: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    Constant {
        magnitude: i32,
    },
    Ramp {
        start: i32,
        end: i32,
    },
    Periodic {
        waveform: Waveform,
        magnitude: u32,
        offset: i32,
        phase: u32,
        period_us: u32,
    },
    /// One entry per axis, in the order of [`Effect::axes`].
    Condition {
        kind: ConditionKind,
        axes: Vec<Condition>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Envelope {
    pub attack_level: u32,
    pub attack_us: u32,
    pub fade_level: u32,
    pub fade_us: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub kind: EffectKind,
    /// Microseconds, or [`INFINITE`].
    pub duration_us: u32,
    pub sample_period_us: u32,
    /// 0..=10000.
    pub gain: u32,
    /// Zero-based button that plays the effect.
    pub trigger_button: Option<u8>,
    pub trigger_repeat_us: u32,
    /// Actuators the effect drives.
    pub axes: Vec<Axis>,
    /// Polar direction: hundredths of a degree clockwise from north (the -y direction),
    /// as DirectInput reports `DIEFF_POLAR` directions.
    pub direction: u32,
    pub envelope: Option<Envelope>,
    pub start_delay_us: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Create or update the effect with this id (the guest's effect object).
    Download {
        id: u32,
        effect: Effect,
    },
    /// Play the effect `iterations` times ([`INFINITE`] loops); `solo` stops the others.
    Start {
        id: u32,
        iterations: u32,
        solo: bool,
    },
    Stop {
        id: u32,
    },
    /// Remove the effect from the device.
    Unload {
        id: u32,
    },
    /// Stop and unload every effect.
    Reset,
    StopAll,
    Pause,
    Continue,
    Actuators(bool),
    /// Device gain 0..=10000.
    Gain(u32),
    Autocenter(bool),
}

impl Command {
    fn effect_id(&self) -> Option<u32> {
        match self {
            Command::Download { id, .. }
            | Command::Start { id, .. }
            | Command::Stop { id }
            | Command::Unload { id } => Some(*id),
            _ => None,
        }
    }
}

/// Receives force-feedback commands from the emulated device.
pub trait ForceSink: Send {
    /// A command issued at virtual time `at_ns`.
    fn send(&mut self, at_ns: u64, cmd: Command);
    fn name(&self) -> String;
}

/// Sends commands to another thread (the host device thread).
pub struct ChannelSink(pub std::sync::mpsc::Sender<Command>);

impl ForceSink for ChannelSink {
    fn send(&mut self, _at_ns: u64, cmd: Command) {
        let _ = self.0.send(cmd);
    }
    fn name(&self) -> String {
        "host joystick".into()
    }
}

/// Writes each command as a line of JSON: `{"t": seconds, "cmd": …}`.
pub struct LogSink(pub std::io::BufWriter<std::fs::File>);

impl ForceSink for LogSink {
    fn send(&mut self, at_ns: u64, cmd: Command) {
        use std::io::Write;
        #[derive(Serialize)]
        struct Line {
            t: f64,
            cmd: Command,
        }
        let line = Line {
            t: at_ns as f64 / 1e9,
            cmd,
        };
        if let Ok(j) = serde_json::to_string(&line) {
            let _ = writeln!(self.0, "{j}");
        }
    }
    fn name(&self) -> String {
        "log".into()
    }
}

/// Sends every command to several sinks.
pub struct Tee(pub Vec<Box<dyn ForceSink>>);

impl ForceSink for Tee {
    fn send(&mut self, at_ns: u64, cmd: Command) {
        for s in &mut self.0 {
            s.send(at_ns, cmd.clone());
        }
    }
    fn name(&self) -> String {
        self.0.iter().map(|s| s.name()).collect::<Vec<_>>().join("+")
    }
}

/// Drop parameter updates that a later update of the same effect replaces, so a guest
/// that changes an effect every frame does not queue up work for a slow device.
pub fn coalesce(cmds: Vec<Command>) -> Vec<Command> {
    let mut keep = vec![true; cmds.len()];
    for (i, c) in cmds.iter().enumerate() {
        let Command::Download { id, .. } = c else { continue };
        for later in &cmds[i + 1..] {
            match later {
                Command::Download { id: l, .. } if l == id => {
                    keep[i] = false;
                    break;
                }
                // Anything that orders against this effect (or all effects) must see it.
                c if c.effect_id().is_none_or(|l| l == *id) => break,
                _ => {}
            }
        }
    }
    cmds.into_iter().zip(keep).filter(|(_, k)| *k).map(|(c, _)| c).collect()
}

/// Polar direction of a DirectInput cartesian direction vector over the effect's axes.
pub fn polar_from_cartesian(axes: &[Axis], v: &[i32]) -> u32 {
    let mut x = 0f64;
    let mut y = 0f64;
    for (a, d) in axes.iter().zip(v) {
        match a {
            Axis::X => x = *d as f64,
            Axis::Y => y = *d as f64,
        }
    }
    if x == 0.0 && y == 0.0 {
        return 0;
    }
    // 0 is north (-y), 9000 is east (+x).
    let deg = x.atan2(-y).to_degrees();
    ((deg * 100.0).round() as i64).rem_euclid(36000) as u32
}

/// Polar direction of a DirectInput spherical direction (first angle from +x towards +y).
pub fn polar_from_spherical(angle: i32) -> u32 {
    (angle as i64 + 9000).rem_euclid(36000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(m: i32) -> Effect {
        Effect {
            kind: EffectKind::Constant { magnitude: m },
            duration_us: INFINITE,
            sample_period_us: 0,
            gain: 10000,
            trigger_button: None,
            trigger_repeat_us: 0,
            axes: vec![Axis::X, Axis::Y],
            direction: 0,
            envelope: None,
            start_delay_us: 0,
        }
    }

    #[test]
    fn cartesian_directions_match_directinput_compass() {
        let xy = [Axis::X, Axis::Y];
        assert_eq!(polar_from_cartesian(&xy, &[0, -1]), 0);
        assert_eq!(polar_from_cartesian(&xy, &[1, 0]), 9000);
        assert_eq!(polar_from_cartesian(&xy, &[0, 1]), 18000);
        assert_eq!(polar_from_cartesian(&xy, &[-1, 0]), 27000);
        assert_eq!(polar_from_cartesian(&xy, &[1, -1]), 4500);
        assert_eq!(polar_from_cartesian(&[Axis::Y], &[1]), 18000);
        assert_eq!(polar_from_cartesian(&[Axis::X], &[-1]), 27000);
        assert_eq!(polar_from_spherical(0), 9000);
        assert_eq!(polar_from_spherical(27000), 0);
    }

    #[test]
    fn coalesce_keeps_the_last_update_before_each_ordering_command() {
        let cmds = vec![
            Command::Download {
                id: 1,
                effect: constant(1),
            },
            Command::Download {
                id: 2,
                effect: constant(2),
            },
            Command::Download {
                id: 1,
                effect: constant(3),
            },
            Command::Start {
                id: 1,
                iterations: 1,
                solo: false,
            },
            Command::Download {
                id: 1,
                effect: constant(4),
            },
            Command::StopAll,
            Command::Download {
                id: 1,
                effect: constant(5),
            },
        ];
        let out = coalesce(cmds);
        let mags: Vec<_> = out
            .iter()
            .map(|c| match c {
                Command::Download { id, effect } => match effect.kind {
                    EffectKind::Constant { magnitude } => format!("d{id}:{magnitude}"),
                    _ => unreachable!(),
                },
                Command::Start { id, .. } => format!("s{id}"),
                Command::StopAll => "stopall".into(),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(mags, ["d2:2", "d1:3", "s1", "d1:4", "stopall", "d1:5"]);
    }
}
