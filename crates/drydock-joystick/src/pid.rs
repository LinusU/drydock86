//! Force feedback over USB HID PID (Physical Interface Device class), the protocol of
//! the Microsoft SideWinder Force Feedback 2 and most other force-feedback sticks.
//!
//! The driver plays [`Command`]s by writing the device's own reports, which it finds by
//! usage in the report descriptor. Effect parameters are already in the units PID
//! devices use for their physical ranges (magnitudes ±10000, hundredths of a degree),
//! so they are scaled through each field's physical range into its logical range.
//! Effect memory is allocated on the device with Create New Effect / PID Block Load.

use crate::ff::{Axis, Command, ConditionKind, Effect, EffectKind, INFINITE, Waveform};
use crate::hid::{Descriptor, Field, PAGE_GENERIC_DESKTOP, PAGE_ORDINAL, PAGE_PID, Report, ReportKind, usage};
use std::collections::BTreeMap;

const fn pid(id: u32) -> u32 {
    usage(PAGE_PID, id)
}

// Collections (reports).
const SET_EFFECT: u32 = pid(0x21);
const SET_ENVELOPE: u32 = pid(0x5a);
const SET_CONDITION: u32 = pid(0x5f);
const SET_PERIODIC: u32 = pid(0x6e);
const SET_CONSTANT: u32 = pid(0x73);
const SET_RAMP: u32 = pid(0x74);
const EFFECT_OPERATION: u32 = pid(0x77);
const BLOCK_FREE: u32 = pid(0x90);
const DEVICE_CONTROL: u32 = pid(0x96);
const DEVICE_GAIN: u32 = pid(0x7d);
const CREATE_NEW_EFFECT: u32 = pid(0xab);
const BLOCK_LOAD: u32 = pid(0x89);
const POOL: u32 = pid(0x7f);

// Fields.
const EFFECT_BLOCK_INDEX: u32 = pid(0x22);
const PARAMETER_BLOCK_OFFSET: u32 = pid(0x23);
const DURATION: u32 = pid(0x50);
const SAMPLE_PERIOD: u32 = pid(0x51);
const GAIN: u32 = pid(0x52);
const TRIGGER_BUTTON: u32 = pid(0x53);
const TRIGGER_REPEAT: u32 = pid(0x54);
const AXES_ENABLE: u32 = pid(0x55);
const DIRECTION_ENABLE: u32 = pid(0x56);
const DIRECTION: u32 = pid(0x57);
const START_DELAY: u32 = pid(0xa7);
const ATTACK_LEVEL: u32 = pid(0x5b);
const ATTACK_TIME: u32 = pid(0x5c);
const FADE_LEVEL: u32 = pid(0x5d);
const FADE_TIME: u32 = pid(0x5e);
const CP_OFFSET: u32 = pid(0x60);
const POSITIVE_COEFFICIENT: u32 = pid(0x61);
const NEGATIVE_COEFFICIENT: u32 = pid(0x62);
const POSITIVE_SATURATION: u32 = pid(0x63);
const NEGATIVE_SATURATION: u32 = pid(0x64);
const DEAD_BAND: u32 = pid(0x65);
const OFFSET: u32 = pid(0x6f);
const MAGNITUDE: u32 = pid(0x70);
const PHASE: u32 = pid(0x71);
const PERIOD: u32 = pid(0x72);
const RAMP_START: u32 = pid(0x75);
const RAMP_END: u32 = pid(0x76);
const OP_START: u32 = pid(0x79);
const OP_START_SOLO: u32 = pid(0x7a);
const OP_STOP: u32 = pid(0x7b);
const LOOP_COUNT: u32 = pid(0x7c);
const DEVICE_GAIN_VALUE: u32 = pid(0x7e);
const ENABLE_ACTUATORS: u32 = pid(0x97);
const DISABLE_ACTUATORS: u32 = pid(0x98);
const STOP_ALL: u32 = pid(0x99);
const RESET: u32 = pid(0x9a);
const PAUSE: u32 = pid(0x9b);
const CONTINUE: u32 = pid(0x9c);
const LOAD_SUCCESS: u32 = pid(0x8c);
const LOAD_FULL: u32 = pid(0x8d);
const RAM_POOL_SIZE: u32 = pid(0x80);
const SIMULTANEOUS_MAX: u32 = pid(0x83);
const BYTE_COUNT: u32 = usage(PAGE_GENERIC_DESKTOP, 0x3b);
const AXIS_X: u32 = usage(PAGE_GENERIC_DESKTOP, 0x30);
const AXIS_Y: u32 = usage(PAGE_GENERIC_DESKTOP, 0x31);

/// Effect-type usages.
fn type_usage(k: &EffectKind) -> u32 {
    pid(match k {
        EffectKind::Constant { .. } => 0x26,
        EffectKind::Ramp { .. } => 0x27,
        EffectKind::Periodic { waveform, .. } => match waveform {
            Waveform::Square => 0x30,
            Waveform::Sine => 0x31,
            Waveform::Triangle => 0x32,
            Waveform::SawtoothUp => 0x33,
            Waveform::SawtoothDown => 0x34,
        },
        EffectKind::Condition { kind, .. } => match kind {
            ConditionKind::Spring => 0x40,
            ConditionKind::Damper => 0x41,
            ConditionKind::Inertia => 0x42,
            ConditionKind::Friction => 0x43,
        },
    })
}

/// Access to the device's reports.
pub trait Transport {
    /// Send an output report (starting with the id byte when reports are numbered).
    fn write(&mut self, report: &[u8]) -> Result<(), String>;
    fn set_feature(&mut self, report: &[u8]) -> Result<(), String>;
    /// Read a feature report; `buf[0]` holds the report id. Returns the length read.
    fn get_feature(&mut self, buf: &mut [u8]) -> Result<usize, String>;
}

/// Does the descriptor describe a PID force-feedback device?
pub fn supports(d: &Descriptor) -> bool {
    [SET_EFFECT, EFFECT_OPERATION, CREATE_NEW_EFFECT, BLOCK_LOAD]
        .iter()
        .all(|c| d.fields.iter().any(|f| f.collections.contains(c)))
}

struct Slot {
    block: i32,
    ty: u32,
    envelope: bool,
    /// Reports last written for this block; unchanged ones are not sent again.
    last: Vec<Vec<u8>>,
}

/// Spring used for DirectInput's auto-centre, which PID devices do not provide.
fn autocenter_spring() -> Effect {
    let c = crate::ff::Condition {
        offset: 0,
        positive_coefficient: 6000,
        negative_coefficient: 6000,
        positive_saturation: 10000,
        negative_saturation: 10000,
        dead_band: 0,
    };
    Effect {
        kind: EffectKind::Condition {
            kind: ConditionKind::Spring,
            axes: vec![c, c],
        },
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

/// Id of the auto-centre spring in the slot table (guest effect ids are COM object ids,
/// which never reach this value).
const AUTOCENTER_ID: u32 = u32::MAX;

pub struct Pid<T: Transport> {
    desc: Descriptor,
    pub io: T,
    slots: BTreeMap<u32, Slot>,
    autocenter: bool,
    actuators: bool,
    /// The guest's device gain, and the user's strength (percent) that scales it.
    gain: u32,
    strength: u32,
    /// Device facts read at start-up (pool size, simultaneous effects), for diagnostics.
    pub info: Vec<(String, i64)>,
    /// Problems that left force feedback working, for the user.
    pub notes: Vec<String>,
}

/// Builds one report of the device's own layout.
struct Builder<'a> {
    d: &'a Descriptor,
    kind: ReportKind,
    coll: u32,
    r: Report,
}

impl<'a> Builder<'a> {
    fn new(d: &'a Descriptor, kind: ReportKind, coll: u32) -> Option<Builder<'a>> {
        let id = d
            .fields
            .iter()
            .find(|f| f.kind == kind && f.collections.contains(&coll))?
            .report_id;
        Some(Builder {
            d,
            kind,
            coll,
            r: d.new_report(kind, id),
        })
    }

    fn field(&self, coll: u32, u: u32) -> Option<(&'a Field, usize)> {
        self.d.find(self.kind, coll, u)
    }

    fn raw(&mut self, u: u32, v: i32) -> &mut Self {
        if let Some((f, i)) = self.field(self.coll, u) {
            self.r.set(f, i, v);
        }
        self
    }

    fn raw_in(&mut self, coll: u32, u: u32, v: i32) -> &mut Self {
        if let Some((f, i)) = self.field(coll, u).filter(|(f, _)| f.collections.contains(&self.coll)) {
            self.r.set(f, i, v);
        }
        self
    }

    /// A value in DirectInput units whose nominal range is `lo..=hi` (used when the
    /// field declares no physical range).
    fn scaled(&mut self, u: u32, v: i64, lo: i64, hi: i64) -> &mut Self {
        if let Some((f, i)) = self.field(self.coll, u) {
            let l = if f.has_physical() {
                f.to_logical(v)
            } else {
                let (lmin, lmax) = (f.logical_min as i64, f.logical_max as i64);
                let q = (v.clamp(lo, hi) - lo) as f64 * (lmax - lmin) as f64 / (hi - lo) as f64;
                (lmin + q.round() as i64) as i32
            };
            self.r.set(f, i, l);
        }
        self
    }

    fn magnitude(&mut self, u: u32, v: i64) -> &mut Self {
        self.scaled(u, v, -10000, 10000)
    }

    fn level(&mut self, u: u32, v: i64) -> &mut Self {
        self.scaled(u, v, 0, 10000)
    }

    /// Time in microseconds. Fields without a unit are taken to count milliseconds.
    fn time(&mut self, u: u32, us: u32) -> &mut Self {
        if let Some((f, i)) = self.field(self.coll, u) {
            if us == INFINITE {
                // All ones means "forever", as Linux's hid-pidff sends it (0xffff).
                self.r.set_raw(f, i, u32::MAX);
                return self;
            }
            let exp = if f.unit != 0 { f.unit_exponent } else { -3 };
            let phys = us as f64 * 10f64.powi(-6 - exp);
            let mut l = if f.has_physical() {
                f.to_logical(phys.round() as i64)
            } else {
                (phys.round() as i64).clamp(f.logical_min as i64, f.logical_max as i64) as i32
            };
            // A short time is not "none": zero can mean infinite or the default.
            if us > 0 && l == f.logical_min && f.logical_max > l {
                l += 1;
            }
            self.r.set(f, i, l);
        }
        self
    }

    /// Angle in hundredths of a degree.
    fn angle(&mut self, coll: u32, u: u32, v: u32) -> &mut Self {
        if let Some((f, i)) = self.field(coll, u) {
            let l = if f.unit != 0 {
                f.to_logical((v as f64 * 10f64.powi(-2 - f.unit_exponent)).round() as i64)
            } else {
                let (lmin, lmax) = (f.logical_min as f64, f.logical_max as f64 + 1.0);
                ((lmin + v as f64 * (lmax - lmin) / 36000.0).floor() as i32).min(f.logical_max)
            };
            self.r.set(f, i, l);
        }
        self
    }

    /// Select `u` in the array field that offers it.
    fn select(&mut self, u: u32) -> &mut Self {
        if let Some(f) = self.d.find_array(self.kind, self.coll, u) {
            let idx = f.usages.iter().position(|&x| x == u).unwrap() as i32;
            self.r.set(f, 0, f.logical_min + idx);
        }
        self
    }

    fn bytes(&self) -> &[u8] {
        &self.r.data
    }
}

impl<T: Transport> Pid<T> {
    /// Take over the device: read its pool, reset it and enable the actuators.
    /// `strength` (percent) scales every force.
    pub fn new(desc: Descriptor, io: T, strength: u32) -> Result<Pid<T>, String> {
        if !supports(&desc) {
            return Err("no PID force-feedback reports".into());
        }
        let mut p = Pid {
            desc,
            io,
            slots: BTreeMap::new(),
            autocenter: true,
            actuators: true,
            gain: 10000,
            strength: strength.min(100),
            info: Vec::new(),
            notes: Vec::new(),
        };
        p.read_pool();
        p.reset()?;
        p.set_gain(10000)?;
        if let Err(e) = p.set_autocenter(true) {
            p.autocenter = false;
            p.notes.push(format!("auto-centre spring unavailable: {e}"));
        }
        Ok(p)
    }

    /// Read a feature report whose id is in `buf[0]`.
    fn get_feature(&mut self, mut buf: Vec<u8>) -> Result<Report, String> {
        let (id, len) = (buf[0], buf.len());
        let n = self.io.get_feature(&mut buf)?;
        buf.truncate(n.max(1));
        // Should a host return the report without its id byte, put it back.
        if self.desc.numbered && n + 1 == len {
            buf.insert(0, id);
        }
        Ok(Report::from_bytes(self.desc.numbered, buf))
    }

    fn read_pool(&mut self) {
        let Some(b) = Builder::new(&self.desc, ReportKind::Feature, POOL) else {
            return;
        };
        let Ok(r) = self.get_feature(b.bytes().to_vec()) else {
            return;
        };
        for (name, u) in [
            ("ram_pool_size", RAM_POOL_SIZE),
            ("simultaneous_effects_max", SIMULTANEOUS_MAX),
        ] {
            if let Some((f, i)) = self.desc.find(ReportKind::Feature, POOL, u) {
                self.info.push((name.into(), r.get(f, i) as i64));
            }
        }
    }

    fn out(&self, coll: u32) -> Option<Builder<'_>> {
        Builder::new(&self.desc, ReportKind::Output, coll)
    }

    fn send(&mut self, b: Option<Vec<u8>>) -> Result<(), String> {
        match b {
            Some(bytes) => self.io.write(&bytes),
            None => Ok(()),
        }
    }

    fn control(&mut self, op: u32) -> Result<(), String> {
        let b = self.out(DEVICE_CONTROL).map(|mut b| b.select(op).bytes().to_vec());
        self.send(b)
    }

    /// Clear the device's effects and restore the actuator state.
    fn reset(&mut self) -> Result<(), String> {
        self.control(RESET)?;
        self.slots.clear();
        // Give the firmware a moment before the next command.
        std::thread::sleep(std::time::Duration::from_millis(50));
        if self.actuators {
            self.control(ENABLE_ACTUATORS)?;
        }
        Ok(())
    }

    fn set_gain(&mut self, g: u32) -> Result<(), String> {
        self.gain = g;
        let level = g as i64 * self.strength as i64 / 100;
        let b = self
            .out(DEVICE_GAIN)
            .map(|mut b| b.level(DEVICE_GAIN_VALUE, level).bytes().to_vec());
        self.send(b)
    }

    /// Allocate an effect block on the device.
    fn create(&mut self, ty: u32) -> Result<i32, String> {
        let req = Builder::new(&self.desc, ReportKind::Feature, CREATE_NEW_EFFECT)
            .ok_or("no Create New Effect report")?
            .select(ty)
            .raw(BYTE_COUNT, 0)
            .bytes()
            .to_vec();
        self.io.set_feature(&req)?;
        let buf = Builder::new(&self.desc, ReportKind::Feature, BLOCK_LOAD)
            .ok_or("no PID Block Load report")?
            .bytes()
            .to_vec();
        let block = self
            .desc
            .find(ReportKind::Feature, BLOCK_LOAD, EFFECT_BLOCK_INDEX)
            .map(|(f, _)| f.clone());
        let status = self
            .desc
            .find_array(ReportKind::Feature, BLOCK_LOAD, LOAD_SUCCESS)
            .cloned();
        let (Some(block), Some(status)) = (block, status) else {
            return Err("PID Block Load report lacks block index or status".into());
        };
        // The device may need a moment to answer (hid-pidff retries too).
        let mut failed = false;
        for _ in 0..40 {
            let r = self.get_feature(buf.clone())?;
            let sel = r.get(&status, 0) - status.logical_min;
            match status.usages.get(sel as usize) {
                Some(&LOAD_SUCCESS) => return Ok(r.get(&block, 0)),
                Some(&LOAD_FULL) => return Err("device effect memory is full".into()),
                other => failed |= other.is_some(),
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Err(if failed {
            "device failed to load the effect".into()
        } else {
            "device did not answer PID Block Load".into()
        })
    }

    fn download(&mut self, id: u32, e: &Effect) -> Result<(), String> {
        let ty = type_usage(&e.kind);
        // A changed type needs a new block.
        if self.slots.get(&id).is_some_and(|s| s.ty != ty) {
            self.unload(id)?;
        }
        if !self.slots.contains_key(&id) {
            let block = self.create(ty)?;
            self.slots.insert(
                id,
                Slot {
                    block,
                    ty,
                    envelope: false,
                    last: Vec::new(),
                },
            );
        }
        let block = self.slots[&id].block;
        let is_condition = matches!(e.kind, EffectKind::Condition { .. });
        // Set Effect, then the type-specific parameters and the envelope, as Linux's
        // hid-pidff sends them.
        let set_effect = self.out(SET_EFFECT).map(|mut b| {
            b.raw(EFFECT_BLOCK_INDEX, block)
                .select(ty)
                .time(DURATION, e.duration_us)
                .time(TRIGGER_REPEAT, e.trigger_repeat_us)
                .time(SAMPLE_PERIOD, e.sample_period_us)
                .level(GAIN, e.gain as i64)
                .time(START_DELAY, e.start_delay_us);
            let trigger = match (e.trigger_button, b.field(SET_EFFECT, TRIGGER_BUTTON)) {
                (Some(n), Some((f, _))) => Some(f.logical_min + n as i32).filter(|&v| v <= f.logical_max),
                _ => None,
            };
            // Out of range is the null value: no trigger.
            b.raw(TRIGGER_BUTTON, trigger.unwrap_or(0));
            if is_condition {
                // One condition block per device axis (see below).
                b.raw_in(AXES_ENABLE, AXIS_X, 1).raw_in(AXES_ENABLE, AXIS_Y, 1);
            } else {
                for a in &e.axes {
                    b.raw_in(AXES_ENABLE, if *a == Axis::X { AXIS_X } else { AXIS_Y }, 1);
                }
                b.raw(DIRECTION_ENABLE, 1)
                    .angle(DIRECTION, usage(PAGE_ORDINAL, 1), e.direction)
                    .angle(DIRECTION, usage(PAGE_ORDINAL, 2), 0);
            }
            b.bytes().to_vec()
        });
        let mut reports: Vec<Vec<u8>> = set_effect.into_iter().collect();
        match &e.kind {
            EffectKind::Constant { magnitude } => {
                reports.extend(self.out(SET_CONSTANT).map(|mut b| {
                    b.raw(EFFECT_BLOCK_INDEX, block)
                        .magnitude(MAGNITUDE, *magnitude as i64)
                        .bytes()
                        .to_vec()
                }));
            }
            EffectKind::Ramp { start, end } => {
                reports.extend(self.out(SET_RAMP).map(|mut b| {
                    b.raw(EFFECT_BLOCK_INDEX, block)
                        .magnitude(RAMP_START, *start as i64)
                        .magnitude(RAMP_END, *end as i64)
                        .bytes()
                        .to_vec()
                }));
            }
            EffectKind::Periodic {
                magnitude,
                offset,
                phase,
                period_us,
                ..
            } => {
                reports.extend(self.out(SET_PERIODIC).map(|mut b| {
                    b.raw(EFFECT_BLOCK_INDEX, block)
                        .level(MAGNITUDE, *magnitude as i64)
                        .magnitude(OFFSET, *offset as i64)
                        .angle(SET_PERIODIC, PHASE, *phase)
                        .time(PERIOD, *period_us)
                        .bytes()
                        .to_vec()
                }));
            }
            EffectKind::Condition { axes, .. } => {
                // Parameter block n is device axis n (X, Y); an axis the effect does not
                // use gets a condition without force.
                let per_axis = [Axis::X, Axis::Y].map(|a| {
                    let k = e.axes.iter().position(|x| *x == a)?;
                    axes.get(k).or(axes.last()).copied()
                });
                for (n, c) in per_axis.iter().enumerate() {
                    let c = c.unwrap_or_default();
                    reports.extend(self.out(SET_CONDITION).map(|mut b| {
                        b.raw(EFFECT_BLOCK_INDEX, block)
                            .raw(PARAMETER_BLOCK_OFFSET, n as i32)
                            .magnitude(CP_OFFSET, c.offset as i64)
                            .magnitude(POSITIVE_COEFFICIENT, c.positive_coefficient as i64)
                            .magnitude(NEGATIVE_COEFFICIENT, c.negative_coefficient as i64)
                            .level(POSITIVE_SATURATION, c.positive_saturation as i64)
                            .level(NEGATIVE_SATURATION, c.negative_saturation as i64)
                            .level(DEAD_BAND, c.dead_band as i64)
                            .bytes()
                            .to_vec()
                    }));
                }
            }
        }
        // Envelopes shape forces only; a block that had one gets a flat one when it goes.
        let had = self.slots[&id].envelope;
        let env = if is_condition { None } else { e.envelope };
        if env.is_some() || had {
            let level = match e.kind {
                EffectKind::Constant { magnitude } => magnitude.unsigned_abs(),
                EffectKind::Ramp { start, end } => start.unsigned_abs().max(end.unsigned_abs()),
                EffectKind::Periodic { magnitude, .. } => magnitude,
                EffectKind::Condition { .. } => 0,
            };
            let env = env.unwrap_or(crate::ff::Envelope {
                attack_level: level,
                attack_us: 0,
                fade_level: level,
                fade_us: 0,
            });
            reports.extend(self.out(SET_ENVELOPE).map(|mut b| {
                b.raw(EFFECT_BLOCK_INDEX, block)
                    .level(ATTACK_LEVEL, env.attack_level as i64)
                    .level(FADE_LEVEL, env.fade_level as i64)
                    .time(ATTACK_TIME, env.attack_us)
                    .time(FADE_TIME, env.fade_us)
                    .bytes()
                    .to_vec()
            }));
            self.slots.get_mut(&id).unwrap().envelope = e.envelope.is_some() && !is_condition;
        }
        // Per-frame updates usually change one report; leave the others alone.
        let last = std::mem::take(&mut self.slots.get_mut(&id).unwrap().last);
        for r in reports.iter().filter(|r| !last.contains(r)) {
            self.io.write(r)?;
        }
        self.slots.get_mut(&id).unwrap().last = reports;
        Ok(())
    }

    fn operate(&mut self, id: u32, op: u32, iterations: u32) -> Result<(), String> {
        let Some(block) = self.slots.get(&id).map(|s| s.block) else {
            return Ok(());
        };
        let b = self.out(EFFECT_OPERATION).map(|mut b| {
            let max = b
                .field(EFFECT_OPERATION, LOOP_COUNT)
                .map(|(f, _)| f.logical_max)
                .unwrap_or(255);
            let n = if iterations == INFINITE {
                max
            } else {
                iterations.min(max.max(0) as u32) as i32
            };
            b.raw(EFFECT_BLOCK_INDEX, block)
                .select(op)
                .raw(LOOP_COUNT, n)
                .bytes()
                .to_vec()
        });
        self.send(b)
    }

    fn unload(&mut self, id: u32) -> Result<(), String> {
        let Some(slot) = self.slots.remove(&id) else {
            return Ok(());
        };
        let b = self
            .out(BLOCK_FREE)
            .map(|mut b| b.raw(EFFECT_BLOCK_INDEX, slot.block).bytes().to_vec());
        self.send(b)
    }

    fn set_autocenter(&mut self, on: bool) -> Result<(), String> {
        self.autocenter = on;
        if on && !self.slots.contains_key(&AUTOCENTER_ID) {
            self.download(AUTOCENTER_ID, &autocenter_spring())?;
            self.operate(AUTOCENTER_ID, OP_START, INFINITE)?;
        } else if !on {
            self.operate(AUTOCENTER_ID, OP_STOP, 0)?;
            self.unload(AUTOCENTER_ID)?;
        }
        Ok(())
    }

    pub fn command(&mut self, c: Command) -> Result<(), String> {
        match c {
            Command::Download { id, effect } => self.download(id, &effect),
            Command::Start { id, iterations, solo } => {
                self.operate(id, if solo { OP_START_SOLO } else { OP_START }, iterations)?;
                // Solo play stops the auto-centre spring too; DirectInput keeps it.
                if solo && self.autocenter {
                    self.operate(AUTOCENTER_ID, OP_START, INFINITE)?;
                }
                Ok(())
            }
            Command::Stop { id } => self.operate(id, OP_STOP, 0),
            Command::Unload { id } => self.unload(id),
            Command::Reset => {
                self.reset()?;
                if self.autocenter {
                    self.set_autocenter(true)?;
                }
                Ok(())
            }
            Command::StopAll => {
                self.control(STOP_ALL)?;
                if self.autocenter {
                    self.operate(AUTOCENTER_ID, OP_START, INFINITE)?;
                }
                Ok(())
            }
            Command::Pause => self.control(PAUSE),
            Command::Continue => self.control(CONTINUE),
            Command::Actuators(on) => {
                self.actuators = on;
                self.control(if on { ENABLE_ACTUATORS } else { DISABLE_ACTUATORS })
            }
            Command::Gain(g) => self.set_gain(g),
            Command::Autocenter(on) => self.set_autocenter(on),
        }
    }

    /// Stop everything and leave the device as a plain joystick.
    pub fn release(&mut self) {
        let _ = self.control(RESET);
        self.slots.clear();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ff::{Condition, Envelope};
    use crate::hid::sidewinder_ff2;

    /// A device with the SideWinder Force Feedback 2's reports that hands out effect
    /// blocks 2, 3, … in order.
    #[derive(Default)]
    pub struct FakeStick {
        pub writes: Vec<Vec<u8>>,
        pub features: Vec<Vec<u8>>,
        next_block: u8,
    }

    impl Transport for FakeStick {
        fn write(&mut self, r: &[u8]) -> Result<(), String> {
            self.writes.push(r.to_vec());
            Ok(())
        }
        fn set_feature(&mut self, r: &[u8]) -> Result<(), String> {
            self.features.push(r.to_vec());
            Ok(())
        }
        fn get_feature(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            match buf[0] {
                2 => {
                    if self.next_block == 0 {
                        self.next_block = 2;
                    }
                    // Block index, status 1 (success), RAM pool available.
                    buf[1] = self.next_block;
                    buf[2] = 1;
                    buf[3] = 0x00;
                    buf[4] = 0x10;
                    self.next_block += 1;
                    Ok(5)
                }
                3 => {
                    buf[1..5].copy_from_slice(&[0xff, 0xff, 0x0a, 0x00]);
                    Ok(5)
                }
                _ => Err("unknown feature report".into()),
            }
        }
    }

    fn effect(kind: EffectKind) -> Effect {
        Effect {
            kind,
            duration_us: 500_000,
            sample_period_us: 0,
            gain: 10000,
            trigger_button: None,
            trigger_repeat_us: 0,
            axes: vec![Axis::X, Axis::Y],
            direction: 9000,
            envelope: None,
            start_delay_us: 0,
        }
    }

    fn stick() -> Pid<FakeStick> {
        let mut p = Pid::new(sidewinder_ff2(), FakeStick::default(), 100).unwrap();
        p.io.writes.clear();
        p.io.features.clear();
        p
    }

    #[test]
    fn start_up_resets_enables_and_centres() {
        let p = Pid::new(sidewinder_ff2(), FakeStick::default(), 100).unwrap();
        assert_eq!(
            p.info,
            [
                ("ram_pool_size".into(), 0xffff),
                ("simultaneous_effects_max".into(), 10)
            ]
        );
        let w = &p.io.writes;
        assert_eq!(w[0], [12, 4]); // Device Control: reset
        assert_eq!(w[1], [12, 1]); // Device Control: enable actuators
        assert_eq!(w[2], [13, 255]); // Device Gain: full
        // Auto-centre spring: Create New Effect (spring is the 8th type), Set Effect,
        // two Set Condition reports (axes 0 and 1), then started looping forever.
        assert_eq!(p.io.features[0], [1, 8, 0, 0]);
        assert_eq!(w[3][..3], [1, 2, 8]);
        assert_eq!(w[4][..3], [3, 2, 0x00]);
        assert_eq!(w[5][..3], [3, 2, 0x01]);
        assert_eq!(w[5][3..], [0, 76, 76, 255, 255, 0]);
        assert_eq!(w[6], [10, 2, 1, 255]);
    }

    #[test]
    fn constant_force_reports() {
        let mut p = stick();
        let mut e = effect(EffectKind::Constant { magnitude: -5000 });
        e.envelope = Some(Envelope {
            attack_level: 0,
            attack_us: 100_000,
            fade_level: 2500,
            fade_us: 41_666,
        });
        p.command(Command::Download { id: 7, effect: e }).unwrap();
        assert_eq!(p.io.features[0], [1, 1, 0, 0]);
        let w = &p.io.writes;
        // Set Effect: block, type 1, duration 500 ms, repeat 0, sample 0, gain 255,
        // no trigger, axes X+Y and direction enabled, east (64 of 255), delay 0.
        assert_eq!(w[0], [1, 3, 1, 0xf4, 0x01, 0, 0, 0, 0, 255, 0, 0b111, 64, 0, 0, 0]);
        // Set Constant Force: block 3, magnitude -127 (of ±255) as 16 bits.
        assert_eq!(w[1], [5, 3, 0x81, 0xff]);
        // Set Envelope: attack level 0, fade level 64, attack 100 ms, fade 42 ms.
        assert_eq!(w[2], [2, 3, 0, 64, 100, 0, 42, 0]);
        p.command(Command::Start {
            id: 7,
            iterations: 2,
            solo: false,
        })
        .unwrap();
        assert_eq!(p.io.writes[3], [10, 3, 1, 2]);
        p.command(Command::Stop { id: 7 }).unwrap();
        assert_eq!(p.io.writes[4], [10, 3, 3, 0]);
        p.command(Command::Unload { id: 7 }).unwrap();
        assert_eq!(p.io.writes[5], [11, 3]);
    }

    #[test]
    fn updates_reuse_the_block_and_send_only_changes() {
        let mut p = stick();
        let mut e = effect(EffectKind::Periodic {
            waveform: Waveform::Sine,
            magnitude: 4400,
            offset: -3488,
            phase: 9000,
            period_us: 90_900,
        });
        e.duration_us = INFINITE;
        p.command(Command::Download {
            id: 1,
            effect: e.clone(),
        })
        .unwrap();
        p.command(Command::Download {
            id: 1,
            effect: e.clone(),
        })
        .unwrap();
        assert_eq!(p.io.features.len(), 1, "one block allocated");
        let w = &p.io.writes;
        assert_eq!(w.len(), 2, "the unchanged update sends nothing");
        assert_eq!(w[0][2], 4, "sine is the 4th type");
        assert_eq!(w[0][3..5], [0xff, 0xff], "infinite duration");
        // Set Periodic: magnitude 112/255, offset -45/127, phase 64/255, period 91 ms.
        assert_eq!(w[1], [4, 3, 112, (-45i8) as u8, 64, 91, 0]);
        if let EffectKind::Periodic { magnitude, .. } = &mut e.kind {
            *magnitude = 10000;
        }
        p.command(Command::Download { id: 1, effect: e }).unwrap();
        assert_eq!(p.io.writes[2..], [vec![4, 3, 255, (-45i8) as u8, 64, 91, 0]]);
    }

    #[test]
    fn changing_type_reallocates() {
        let mut p = stick();
        p.command(Command::Download {
            id: 1,
            effect: effect(EffectKind::Constant { magnitude: 1 }),
        })
        .unwrap();
        p.command(Command::Download {
            id: 1,
            effect: effect(EffectKind::Ramp {
                start: -10000,
                end: 10000,
            }),
        })
        .unwrap();
        assert_eq!(p.io.features.len(), 2);
        assert!(p.io.writes.contains(&vec![11, 3]), "old block freed");
        assert!(p.io.writes.contains(&vec![6, 4, 0x80, 0x7f]), "ramp on the new block");
    }

    #[test]
    fn conditions_use_axes_not_direction() {
        let mut p = stick();
        let c = Condition {
            offset: 0,
            positive_coefficient: 10000,
            negative_coefficient: 10000,
            positive_saturation: 10000,
            negative_saturation: 10000,
            dead_band: 0,
        };
        let mut e = effect(EffectKind::Condition {
            kind: ConditionKind::Damper,
            axes: vec![c],
        });
        e.axes = vec![Axis::Y];
        p.command(Command::Download { id: 1, effect: e }).unwrap();
        let w = &p.io.writes;
        assert_eq!(w.len(), 3, "Set Effect, then a Set Condition per device axis");
        assert_eq!(w[0][11], 0b011, "both axes enabled, direction disabled");
        assert_eq!(w[1][2..], [0x00, 0, 0, 0, 0, 0, 0], "X: no force");
        assert_eq!(w[2][2..], [0x01, 0, 127, 127, 255, 255, 0], "Y: the damper");
    }

    #[test]
    fn strength_scales_the_device_gain() {
        let mut p = Pid::new(sidewinder_ff2(), FakeStick::default(), 50).unwrap();
        assert_eq!(p.io.writes[2], [13, 128]);
        p.command(Command::Gain(5000)).unwrap();
        assert_eq!(p.io.writes.last().unwrap(), &vec![13, 64]);
    }

    #[test]
    fn autocenter_off_frees_the_spring_and_reset_restores_it() {
        let mut p = stick();
        p.command(Command::Autocenter(false)).unwrap();
        assert_eq!(p.io.writes, [vec![10, 2, 3, 0], vec![11, 2]]);
        p.command(Command::Autocenter(true)).unwrap();
        p.io.writes.clear();
        p.command(Command::Reset).unwrap();
        assert_eq!(p.io.writes[0], [12, 4]);
        assert_eq!(p.io.writes[1], [12, 1]);
        assert_eq!(p.io.writes.last().unwrap(), &vec![10, 4, 1, 255]);
    }
}
