//! DirectInput force feedback: effects (IDirectInputEffect) and the joystick's
//! force-feedback state.
//!
//! The guest-visible behaviour (parameters, download state, playing status) is kept
//! here on virtual time. Downloads, starts and device commands are also sent to the
//! host's [`drydock_joystick::ff::ForceSink`] when one is attached; nothing comes back,
//! so a run plays the same with or without force-feedback hardware.

use super::dinput::{self, DI_OK, DIERR_DEVICENOTREG, DIERR_INVALIDPARAM, DIERR_UNSUPPORTED, DevKind, DiDevice, Obj};
use super::*;
use crate::{export, iunknown};
use drydock_joystick::ff::{self, Command, ConditionKind, EffectKind, Envelope, INFINITE, Waveform};
use serde::{Deserialize, Serialize};

pub const DI_NOEFFECT: u32 = 1;
pub const DI_DOWNLOADSKIPPED: u32 = 3;
pub const DIERR_MOREDATA: u32 = 0x8004_0202;
pub const DIERR_NOTDOWNLOADED: u32 = 0x8004_0203;
pub const DIERR_NOTEXCLUSIVEACQUIRED: u32 = 0x8004_0205;
pub const DIERR_INCOMPLETEEFFECT: u32 = 0x8004_0206;

// DIEFFECT.dwFlags (objects are given by id unless DIEFF_OBJECTOFFSETS)
const DIEFF_OBJECTOFFSETS: u32 = 0x02;
const DIEFF_CARTESIAN: u32 = 0x10;
const DIEFF_POLAR: u32 = 0x20;
const DIEFF_SPHERICAL: u32 = 0x40;
const DIEFF_COORDS: u32 = DIEFF_CARTESIAN | DIEFF_POLAR | DIEFF_SPHERICAL;

// SetParameters / GetParameters flags
const DIEP_DURATION: u32 = 0x001;
const DIEP_SAMPLEPERIOD: u32 = 0x002;
const DIEP_GAIN: u32 = 0x004;
const DIEP_TRIGGERBUTTON: u32 = 0x008;
const DIEP_TRIGGERREPEATINTERVAL: u32 = 0x010;
const DIEP_AXES: u32 = 0x020;
const DIEP_DIRECTION: u32 = 0x040;
const DIEP_ENVELOPE: u32 = 0x080;
const DIEP_TYPESPECIFICPARAMS: u32 = 0x100;
const DIEP_STARTDELAY: u32 = 0x200;
const DIEP_ALLPARAMS_DX5: u32 = 0x1ff;
const DIEP_ALLPARAMS: u32 = 0x3ff;
const DIEP_START: u32 = 0x2000_0000;
const DIEP_NODOWNLOAD: u32 = 0x8000_0000;

const DIEB_NOTRIGGER: u32 = 0xffff_ffff;
const DIES_SOLO: u32 = 0x1;
const DIES_NODOWNLOAD: u32 = 0x8000_0000;
const DIEGES_PLAYING: u32 = 0x1;

// SendForceFeedbackCommand
const DISFFC_RESET: u32 = 0x01;
const DISFFC_STOPALL: u32 = 0x02;
const DISFFC_PAUSE: u32 = 0x04;
const DISFFC_CONTINUE: u32 = 0x08;
const DISFFC_SETACTUATORSON: u32 = 0x10;
const DISFFC_SETACTUATORSOFF: u32 = 0x20;

// GetForceFeedbackState
const DIGFFS_EMPTY: u32 = 0x01;
const DIGFFS_STOPPED: u32 = 0x02;
const DIGFFS_PAUSED: u32 = 0x04;
const DIGFFS_ACTUATORSON: u32 = 0x10;
const DIGFFS_ACTUATORSOFF: u32 = 0x20;
const DIGFFS_POWERON: u32 = 0x40;
const DIGFFS_SAFETYSWITCHON: u32 = 0x100;
const DIGFFS_USERFFSWITCHON: u32 = 0x400;

// DIEFFECTINFO.dwEffType
const DIEFT_CONSTANTFORCE: u32 = 0x01;
const DIEFT_RAMPFORCE: u32 = 0x02;
const DIEFT_PERIODIC: u32 = 0x03;
const DIEFT_CONDITION: u32 = 0x04;
const DIEFT_FFATTACK: u32 = 0x200;
const DIEFT_FFFADE: u32 = 0x400;
const DIEFT_SATURATION: u32 = 0x800;
const DIEFT_POSNEGCOEFFICIENTS: u32 = 0x1000;
const DIEFT_POSNEGSATURATION: u32 = 0x2000;
const DIEFT_DEADBAND: u32 = 0x4000;
const DIEFT_STARTDELAY: u32 = 0x8000;

/// Effects the joystick offers, in enumeration order.
const EFFECTS: &[(Guid, Base, &str)] = &[
    (
        guid("13541c20-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Constant,
        "Constant Force",
    ),
    (guid("13541c21-8e33-11d0-9ad0-00a0c9a06e35"), Base::Ramp, "Ramp Force"),
    (
        guid("13541c22-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Periodic(Waveform::Square),
        "Square",
    ),
    (
        guid("13541c23-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Periodic(Waveform::Sine),
        "Sine",
    ),
    (
        guid("13541c24-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Periodic(Waveform::Triangle),
        "Triangle",
    ),
    (
        guid("13541c25-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Periodic(Waveform::SawtoothUp),
        "Sawtooth Up",
    ),
    (
        guid("13541c26-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Periodic(Waveform::SawtoothDown),
        "Sawtooth Down",
    ),
    (
        guid("13541c27-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Condition(ConditionKind::Spring),
        "Spring",
    ),
    (
        guid("13541c28-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Condition(ConditionKind::Damper),
        "Damper",
    ),
    (
        guid("13541c29-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Condition(ConditionKind::Inertia),
        "Inertia",
    ),
    (
        guid("13541c2a-8e33-11d0-9ad0-00a0c9a06e35"),
        Base::Condition(ConditionKind::Friction),
        "Friction",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    Constant,
    Ramp,
    Periodic(Waveform),
    Condition(ConditionKind),
}

fn base_of(g: &Guid) -> Option<Base> {
    EFFECTS.iter().find(|(eg, _, _)| eg == g).map(|(_, b, _)| *b)
}

fn eff_type(b: Base) -> u32 {
    let force = DIEFT_FFATTACK | DIEFT_FFFADE | DIEFT_STARTDELAY;
    match b {
        Base::Constant => DIEFT_CONSTANTFORCE | force,
        Base::Ramp => DIEFT_RAMPFORCE | force,
        Base::Periodic(_) => DIEFT_PERIODIC | force,
        Base::Condition(_) => {
            DIEFT_CONDITION
                | DIEFT_SATURATION
                | DIEFT_POSNEGCOEFFICIENTS
                | DIEFT_POSNEGSATURATION
                | DIEFT_DEADBAND
                | DIEFT_STARTDELAY
        }
    }
}

/// Size of the type-specific parameters for `axes` axes.
fn params_ok(b: Base, cb: u32, axes: usize) -> bool {
    match b {
        Base::Constant => cb == 4,
        Base::Ramp => cb == 8,
        Base::Periodic(_) => cb == 16,
        // One DICONDITION for all axes, or one per axis.
        Base::Condition(_) => cb == 24 || (axes > 1 && cb == 24 * axes as u32),
    }
}

/// The joystick's force-feedback state (DirectInput's view of it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfState {
    pub gain: u32,
    pub autocenter: bool,
    pub paused: bool,
    pub actuators: bool,
}

impl Default for FfState {
    fn default() -> Self {
        FfState {
            gain: 10000,
            autocenter: true,
            paused: false,
            actuators: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiEffect {
    pub device: u32,
    pub guid: Guid,
    pub duration: u32,
    pub sample_period: u32,
    pub gain: u32,
    pub trigger_button: Option<Obj>,
    pub trigger_repeat: u32,
    pub axes: Vec<Obj>,
    /// Coordinate system of the direction as the program last set it.
    pub dir_flags: u32,
    pub direction: Vec<i32>,
    pub envelope: Option<Envelope>,
    /// Type-specific parameters as the program passed them.
    pub params: Vec<u8>,
    pub start_delay: u32,
    /// DIEP_* parameters that have been set.
    pub set: u32,
    pub downloaded: bool,
    /// (start time, iterations) of the current play.
    pub playing: Option<(u64, u32)>,
}

impl DiEffect {
    fn base(&self) -> Base {
        base_of(&self.guid).expect("effects are created from known GUIDs")
    }

    /// Everything a download needs is there.
    fn complete(&self) -> bool {
        !self.axes.is_empty() && self.set & DIEP_TYPESPECIFICPARAMS != 0
    }

    fn is_playing(&self, now: u64) -> bool {
        let Some((start, iterations)) = self.playing else {
            return false;
        };
        if self.duration == INFINITE || iterations == INFINITE {
            return true;
        }
        let end = start + self.start_delay as u64 * 1000 + iterations as u64 * self.duration as u64 * 1000;
        now < end
    }

    fn polar(&self) -> u32 {
        let axes: Vec<ff::Axis> = self.axes.iter().map(|o| ff_axis(*o)).collect();
        let d0 = self.direction.first().copied().unwrap_or(0);
        if axes.len() == 1 {
            // A one-axis effect points along its axis; only the sign counts.
            return ff::polar_from_cartesian(&axes, &[if d0 < 0 { -1 } else { 1 }]);
        }
        match self.dir_flags & DIEFF_COORDS {
            DIEFF_POLAR => d0.rem_euclid(36000) as u32,
            DIEFF_SPHERICAL => ff::polar_from_spherical(d0),
            _ => ff::polar_from_cartesian(&axes, &self.direction),
        }
    }

    /// The effect as the host's actuators take it.
    fn to_ff(&self) -> ff::Effect {
        let p = &self.params;
        let rd = |i: usize| i32::from_le_bytes(p[i * 4..i * 4 + 4].try_into().unwrap());
        let kind = match self.base() {
            Base::Constant => EffectKind::Constant { magnitude: rd(0) },
            Base::Ramp => EffectKind::Ramp {
                start: rd(0),
                end: rd(1),
            },
            Base::Periodic(waveform) => EffectKind::Periodic {
                waveform,
                magnitude: rd(0) as u32,
                offset: rd(1),
                phase: rd(2) as u32,
                period_us: rd(3) as u32,
            },
            Base::Condition(kind) => {
                let n = p.len() / 24;
                let axes = (0..self.axes.len())
                    .map(|a| {
                        let o = 6 * a.min(n - 1);
                        ff::Condition {
                            offset: rd(o),
                            positive_coefficient: rd(o + 1),
                            negative_coefficient: rd(o + 2),
                            positive_saturation: rd(o + 3) as u32,
                            negative_saturation: rd(o + 4) as u32,
                            dead_band: rd(o + 5),
                        }
                    })
                    .collect();
                EffectKind::Condition { kind, axes }
            }
        };
        ff::Effect {
            kind,
            duration_us: self.duration,
            sample_period_us: self.sample_period,
            gain: self.gain,
            trigger_button: match self.trigger_button {
                Some(Obj::Button(n)) => Some(n as u8),
                _ => None,
            },
            trigger_repeat_us: self.trigger_repeat,
            axes: self.axes.iter().map(|o| ff_axis(*o)).collect(),
            direction: self.polar(),
            envelope: self.envelope,
            start_delay_us: self.start_delay,
        }
    }
}

fn ff_axis(o: Obj) -> ff::Axis {
    if o == Obj::Axis(1) { ff::Axis::Y } else { ff::Axis::X }
}

fn send(s: &mut Session, c: Command) {
    let now = s.clock.now_ns();
    if let Some(sink) = s.host.ff.as_mut() {
        sink.send(now, c);
    }
}

fn device(s: &Session, id: u32) -> Option<&DiDevice> {
    match &s.com_obj(id)?.kind {
        ComKind::DInputDevice(d) => Some(d),
        _ => None,
    }
}

fn device_mut(s: &mut Session, id: u32) -> &mut DiDevice {
    match &mut s.com_obj_mut(id).unwrap().kind {
        ComKind::DInputDevice(d) => d,
        _ => unreachable!(),
    }
}

fn effect(s: &Session, id: u32) -> &DiEffect {
    match &s.com_obj(id).unwrap().kind {
        ComKind::DInputEffect(e) => e,
        _ => unreachable!(),
    }
}

fn effect_mut(s: &mut Session, id: u32) -> &mut DiEffect {
    match &mut s.com_obj_mut(id).unwrap().kind {
        ComKind::DInputEffect(e) => e,
        _ => unreachable!(),
    }
}

fn this_effect(s: &Session) -> Result<u32, ApiError> {
    let id = s.this_obj()?;
    match s.com_obj(id).map(|o| &o.kind) {
        Some(ComKind::DInputEffect(_)) => Ok(id),
        _ => Err(ApiError::Fatal("not a DirectInput effect".into())),
    }
}

/// Effects belong to the device; it needs to be acquired exclusively to play them.
fn exclusive(s: &Session, dev: u32) -> bool {
    device(s, dev).is_some_and(|d| d.acquired && d.coop & 1 != 0)
}

fn effects_of(s: &Session, dev: u32) -> Vec<u32> {
    s.com
        .objs
        .iter()
        .filter(|(_, o)| matches!(&o.kind, ComKind::DInputEffect(e) if e.device == dev))
        .map(|(k, _)| *k)
        .collect()
}

pub fn effect_destroyed(s: &mut Session, id: u32, e: DiEffect) {
    if e.downloaded {
        send(s, Command::Unload { id });
    }
    s.com_release(e.device);
}

/// Tell a newly attached sink what the guest has already set up (after a restore,
/// or when the sink arrives late): device state, downloaded effects, playing loops.
pub fn resync(s: &mut Session) {
    let now = s.clock.now_ns();
    let devices: Vec<(u32, FfState)> = s
        .com
        .objs
        .iter()
        .filter_map(|(id, o)| match &o.kind {
            ComKind::DInputDevice(d) if d.kind == DevKind::Joystick => Some((*id, d.ff.clone())),
            _ => None,
        })
        .collect();
    for (dev, st) in devices {
        send(s, Command::Gain(st.gain));
        send(s, Command::Autocenter(st.autocenter));
        send(s, Command::Actuators(st.actuators));
        for id in effects_of(s, dev) {
            let e = effect(s, id).clone();
            if !e.downloaded {
                continue;
            }
            send(s, Command::Download { id, effect: e.to_ff() });
            // Restart what is still playing; finite plays restart from the beginning.
            if let Some((_, iterations)) = e.playing.filter(|_| e.is_playing(now)) {
                send(
                    s,
                    Command::Start {
                        id,
                        iterations,
                        solo: false,
                    },
                );
            }
        }
    }
}

/// The device lost exclusive access: its effects stop and leave the hardware.
pub fn on_unacquire(s: &mut Session, dev: u32) {
    for id in effects_of(s, dev) {
        let e = effect_mut(s, id);
        e.playing = None;
        if std::mem::replace(&mut e.downloaded, false) {
            send(s, Command::Unload { id });
        }
    }
}

/// DIPROP_FFGAIN / DIPROP_AUTOCENTER.
pub fn set_property(s: &mut Session, dev: u32, prop: u32, v: u32) -> u32 {
    match prop {
        7 => {
            if v > 10000 {
                return DIERR_INVALIDPARAM;
            }
            device_mut(s, dev).ff.gain = v;
            send(s, Command::Gain(v));
        }
        _ => {
            if v > 1 {
                return DIERR_INVALIDPARAM;
            }
            device_mut(s, dev).ff.autocenter = v == 1;
            send(s, Command::Autocenter(v == 1));
        }
    }
    DI_OK
}

/// DIPROP_FFGAIN, DIPROP_FFLOAD, DIPROP_AUTOCENTER.
pub fn get_property(s: &Session, dev: u32, prop: u32) -> Result<u32, u32> {
    let d = device(s, dev).ok_or(DIERR_INVALIDPARAM)?;
    match prop {
        7 => Ok(d.ff.gain),
        8 => {
            if !exclusive(s, dev) {
                return Err(DIERR_NOTEXCLUSIVEACQUIRED);
            }
            // Percentage of the 40 effect blocks in use.
            let used = effects_of(s, dev).iter().filter(|&&e| effect(s, e).downloaded).count();
            Ok((used * 100 / 40).min(100) as u32)
        }
        _ => Ok(d.ff.autocenter as u32),
    }
}

/// Resolve an object given by offset or id (per DIEFF_OBJECT* flags).
fn resolve(d: &DiDevice, flags: u32, v: u32) -> Option<Obj> {
    if flags & DIEFF_OBJECTOFFSETS != 0 {
        d.format.iter().find(|f| f.ofs == v).map(|f| f.obj)
    } else {
        dinput::obj_by_id(d.kind, v)
    }
}

fn describe(d: &DiDevice, flags: u32, o: Obj) -> u32 {
    if flags & DIEFF_OBJECTOFFSETS != 0 {
        d.format.iter().find(|f| f.obj == o).map(|f| f.ofs).unwrap_or(0)
    } else {
        dinput::obj_type(d.kind, o, d.rel_axes)
    }
}

/// SetParameters on effect `id`; also used by CreateEffect.
fn set_params(s: &mut Session, id: u32, p: u32, flags: u32) -> Result<u32, ApiError> {
    let mut e = effect(s, id).clone();
    let Some(d) = device(s, e.device).cloned() else {
        return Ok(DIERR_INVALIDPARAM);
    };
    let size = s.mem.read_u32(p)?;
    if size != 52 && size != 56 {
        return Ok(DIERR_INVALIDPARAM);
    }
    let r = |o: u32| s.mem.read_u32(p + o);
    let eflags = r(4)?;
    if flags & DIEP_DURATION != 0 {
        e.duration = r(8)?;
    }
    if flags & DIEP_SAMPLEPERIOD != 0 {
        e.sample_period = r(12)?;
    }
    if flags & DIEP_GAIN != 0 {
        e.gain = r(16)?;
        if e.gain > 10000 {
            return Ok(DIERR_INVALIDPARAM);
        }
    }
    if flags & DIEP_TRIGGERBUTTON != 0 {
        let b = r(20)?;
        e.trigger_button = if b == DIEB_NOTRIGGER {
            None
        } else {
            match resolve(&d, eflags, b) {
                Some(o @ Obj::Button(_)) => Some(o),
                _ => return Ok(DIERR_INVALIDPARAM),
            }
        };
    }
    if flags & DIEP_TRIGGERREPEATINTERVAL != 0 {
        e.trigger_repeat = r(24)?;
    }
    let n = r(28)?;
    if flags & (DIEP_AXES | DIEP_DIRECTION) != 0 && !(1..=2).contains(&n) {
        return Ok(DIERR_INVALIDPARAM);
    }
    if flags & DIEP_AXES != 0 {
        let pa = r(32)?;
        let mut axes = Vec::new();
        for i in 0..n {
            match resolve(&d, eflags, s.mem.read_u32(pa + 4 * i)?) {
                // The stick's actuators drive X and Y.
                Some(o @ (Obj::Axis(0) | Obj::Axis(1))) if !axes.contains(&o) => axes.push(o),
                _ => return Ok(DIERR_INVALIDPARAM),
            }
        }
        e.axes = axes;
    }
    if flags & DIEP_DIRECTION != 0 {
        let coords = eflags & DIEFF_COORDS;
        if coords.count_ones() != 1 {
            return Ok(DIERR_INVALIDPARAM);
        }
        let pd = r(36)?;
        e.dir_flags = coords;
        e.direction = (0..n).map(|i| s.mem.read_i32(pd + 4 * i)).collect::<Result<_, _>>()?;
    }
    if flags & DIEP_ENVELOPE != 0 {
        let pe = r(40)?;
        e.envelope = if pe == 0 {
            None
        } else {
            let v = |o: u32| s.mem.read_u32(pe + o);
            if v(0)? != 20 {
                return Ok(DIERR_INVALIDPARAM);
            }
            Some(Envelope {
                attack_level: v(4)?,
                attack_us: v(8)?,
                fade_level: v(12)?,
                fade_us: v(16)?,
            })
        };
    }
    if flags & DIEP_TYPESPECIFICPARAMS != 0 {
        let (cb, pt) = (r(44)?, r(48)?);
        if !params_ok(e.base(), cb, e.axes.len().max(1)) || pt == 0 {
            return Ok(DIERR_INVALIDPARAM);
        }
        e.params = s.mem.read_vec(pt, cb)?;
    }
    if flags & DIEP_STARTDELAY != 0 && size >= 56 {
        e.start_delay = r(52)?;
    }
    e.set |= flags & DIEP_ALLPARAMS;
    *effect_mut(s, id) = e;
    if flags & DIEP_NODOWNLOAD != 0 || !exclusive(s, d_id(s, id)) {
        return Ok(DI_DOWNLOADSKIPPED);
    }
    if !effect(s, id).complete() {
        return Ok(DI_OK);
    }
    download(s, id);
    if flags & DIEP_START != 0 {
        return Ok(start(s, id, 1, 0));
    }
    Ok(DI_OK)
}

fn d_id(s: &Session, id: u32) -> u32 {
    effect(s, id).device
}

fn download(s: &mut Session, id: u32) {
    let fe = effect(s, id).to_ff();
    effect_mut(s, id).downloaded = true;
    send(s, Command::Download { id, effect: fe });
}

fn start(s: &mut Session, id: u32, iterations: u32, flags: u32) -> u32 {
    let dev = d_id(s, id);
    if !exclusive(s, dev) {
        return DIERR_NOTEXCLUSIVEACQUIRED;
    }
    if iterations == 0 {
        return DIERR_INVALIDPARAM;
    }
    if !effect(s, id).downloaded {
        if flags & DIES_NODOWNLOAD != 0 {
            return DIERR_NOTDOWNLOADED;
        }
        if !effect(s, id).complete() {
            return DIERR_INCOMPLETEEFFECT;
        }
        download(s, id);
    }
    let solo = flags & DIES_SOLO != 0;
    if solo {
        for other in effects_of(s, dev) {
            effect_mut(s, other).playing = None;
        }
    }
    let now = s.clock.now_ns();
    effect_mut(s, id).playing = Some((now, iterations));
    send(s, Command::Start { id, iterations, solo });
    DI_OK
}

// ---------------------------------------------------------------- device methods

pub fn dev_create_effect(s: &mut Session) -> ApiResult {
    let [_this, rguid, peff, pp, outer] = s.args::<5>()?;
    let dev = s.this_obj()?;
    if device(s, dev).is_none_or(|d| d.kind != DevKind::Joystick) {
        return Ok(ApiRet::Ret(DIERR_UNSUPPORTED));
    }
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    if pp == 0 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    let g = s.read_guid(rguid)?;
    if base_of(&g).is_none() {
        s.missing("DirectInput", format!("CreateEffect {}", guid_str(&g)));
        return Ok(ApiRet::Ret(DIERR_DEVICENOTREG));
    }
    let e = DiEffect {
        device: dev,
        guid: g,
        duration: 0,
        sample_period: 0,
        gain: 10000,
        trigger_button: None,
        trigger_repeat: 0,
        axes: Vec::new(),
        dir_flags: DIEFF_CARTESIAN,
        direction: Vec::new(),
        envelope: None,
        params: Vec::new(),
        start_delay: 0,
        set: 0,
        downloaded: false,
        playing: None,
    };
    let id = s.com_new(ComKind::DInputEffect(Box::new(e)));
    s.com_addref(dev);
    if peff != 0 {
        let all = if s.mem.read_u32(peff)? == 52 {
            DIEP_ALLPARAMS_DX5
        } else {
            DIEP_ALLPARAMS
        };
        let hr = set_params(s, id, peff, all)?;
        if hr & 0x8000_0000 != 0 {
            s.com_release(id);
            s.mem.write_u32(pp, 0)?;
            return Ok(ApiRet::Ret(hr));
        }
    }
    s.out_iface(pp, id, &IDIRECTINPUTEFFECT)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn effect_info(g: &Guid, b: Base, name: &str) -> Vec<u8> {
    let mut v = vec![0u8; 292];
    v[0..4].copy_from_slice(&292u32.to_le_bytes());
    v[4..20].copy_from_slice(g);
    v[20..24].copy_from_slice(&eff_type(b).to_le_bytes());
    v[24..28].copy_from_slice(&DIEP_ALLPARAMS.to_le_bytes());
    v[28..32].copy_from_slice(&DIEP_ALLPARAMS.to_le_bytes());
    v[32..32 + name.len()].copy_from_slice(name.as_bytes());
    v
}

pub fn dev_enum_effects(s: &mut Session) -> ApiResult {
    let [_this, cb, ctx, ty] = s.args::<4>()?;
    let (_, d) = dinput::dev_self(s)?;
    if d.kind != DevKind::Joystick {
        return Ok(ApiRet::Ret(DI_OK));
    }
    let items = EFFECTS
        .iter()
        .filter(|(_, b, _)| ty & 0xff == 0 || eff_type(*b) & 0xff == ty & 0xff)
        .map(|(g, b, n)| vec![CallArg::Buf(effect_info(g, *b, n)), CallArg::Val(ctx)])
        .collect();
    enum_callbacks(cb, items, 0, DI_OK)
}

pub fn dev_get_effect_info(s: &mut Session) -> ApiResult {
    let [_this, p, rguid] = s.args::<3>()?;
    let (_, d) = dinput::dev_self(s)?;
    let g = s.read_guid(rguid)?;
    let found = EFFECTS.iter().find(|(eg, _, _)| *eg == g);
    match found {
        Some((g, b, n)) if d.kind == DevKind::Joystick => {
            if s.mem.read_u32(p)? != 292 {
                return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
            }
            s.mem.write(p, &effect_info(g, *b, n))?;
            Ok(ApiRet::Ret(DI_OK))
        }
        _ => Ok(ApiRet::Ret(DIERR_DEVICENOTREG)),
    }
}

pub fn dev_get_ff_state(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (dev, d) = dinput::dev_self(s)?;
    if d.kind != DevKind::Joystick {
        return Ok(ApiRet::Ret(DIERR_UNSUPPORTED));
    }
    if !exclusive(s, dev) {
        return Ok(ApiRet::Ret(DIERR_NOTEXCLUSIVEACQUIRED));
    }
    let now = s.clock.now_ns();
    let effects = effects_of(s, dev);
    let mut st = DIGFFS_POWERON | DIGFFS_SAFETYSWITCHON | DIGFFS_USERFFSWITCHON;
    if !effects.iter().any(|&e| effect(s, e).downloaded) {
        st |= DIGFFS_EMPTY;
    }
    if !effects.iter().any(|&e| effect(s, e).is_playing(now)) {
        st |= DIGFFS_STOPPED;
    }
    if d.ff.paused {
        st |= DIGFFS_PAUSED;
    }
    st |= if d.ff.actuators {
        DIGFFS_ACTUATORSON
    } else {
        DIGFFS_ACTUATORSOFF
    };
    s.mem.write_u32(p, st)?;
    Ok(ApiRet::Ret(DI_OK))
}

pub fn dev_send_ff_command(s: &mut Session) -> ApiResult {
    let [_this, flags] = s.args::<2>()?;
    let (dev, d) = dinput::dev_self(s)?;
    if d.kind != DevKind::Joystick {
        return Ok(ApiRet::Ret(DIERR_UNSUPPORTED));
    }
    if flags == 0 || flags & !0x3f != 0 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    if !exclusive(s, dev) {
        return Ok(ApiRet::Ret(DIERR_NOTEXCLUSIVEACQUIRED));
    }
    if flags & DISFFC_RESET != 0 {
        for e in effects_of(s, dev) {
            let e = effect_mut(s, e);
            e.playing = None;
            e.downloaded = false;
        }
        device_mut(s, dev).ff.paused = false;
        send(s, Command::Reset);
    }
    if flags & DISFFC_STOPALL != 0 {
        for e in effects_of(s, dev) {
            effect_mut(s, e).playing = None;
        }
        send(s, Command::StopAll);
    }
    if flags & DISFFC_PAUSE != 0 {
        device_mut(s, dev).ff.paused = true;
        send(s, Command::Pause);
    }
    if flags & DISFFC_CONTINUE != 0 {
        device_mut(s, dev).ff.paused = false;
        send(s, Command::Continue);
    }
    for (bit, on) in [(DISFFC_SETACTUATORSON, true), (DISFFC_SETACTUATORSOFF, false)] {
        if flags & bit != 0 {
            device_mut(s, dev).ff.actuators = on;
            send(s, Command::Actuators(on));
        }
    }
    Ok(ApiRet::Ret(DI_OK))
}

pub fn dev_enum_created_effects(s: &mut Session) -> ApiResult {
    let [_this, cb, ctx, flags] = s.args::<4>()?;
    if flags != 0 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    let (dev, _) = dinput::dev_self(s)?;
    let mut items = Vec::new();
    for e in effects_of(s, dev) {
        let p = s.iface(e, &IDIRECTINPUTEFFECT)?;
        items.push(vec![CallArg::Val(p), CallArg::Val(ctx)]);
    }
    enum_callbacks(cb, items, 0, DI_OK)
}

// ---------------------------------------------------------------- effect methods

fn eff_initialize(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DI_OK))
}

fn eff_get_guid(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let id = this_effect(s)?;
    let g = effect(s, id).guid;
    s.mem.write(p, &g)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn eff_set_parameters(s: &mut Session) -> ApiResult {
    let [_this, p, flags] = s.args::<3>()?;
    let id = this_effect(s)?;
    if p == 0 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    Ok(ApiRet::Ret(set_params(s, id, p, flags)?))
}

fn eff_get_parameters(s: &mut Session) -> ApiResult {
    let [_this, p, flags] = s.args::<3>()?;
    let id = this_effect(s)?;
    let e = effect(s, id).clone();
    let Some(d) = device(s, e.device).cloned() else {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    };
    let size = s.mem.read_u32(p)?;
    if size != 52 && size != 56 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    let eflags = s.mem.read_u32(p + 4)?;
    let mut hr = DI_OK;
    let w = |s: &mut Session, o: u32, v: u32| s.mem.write_u32(p + o, v);
    if flags & DIEP_DURATION != 0 {
        w(s, 8, e.duration)?;
    }
    if flags & DIEP_SAMPLEPERIOD != 0 {
        w(s, 12, e.sample_period)?;
    }
    if flags & DIEP_GAIN != 0 {
        w(s, 16, e.gain)?;
    }
    if flags & DIEP_TRIGGERBUTTON != 0 {
        let v = e
            .trigger_button
            .map(|o| describe(&d, eflags, o))
            .unwrap_or(DIEB_NOTRIGGER);
        w(s, 20, v)?;
    }
    if flags & DIEP_TRIGGERREPEATINTERVAL != 0 {
        w(s, 24, e.trigger_repeat)?;
    }
    if flags & (DIEP_AXES | DIEP_DIRECTION) != 0 {
        let room = s.mem.read_u32(p + 28)?;
        let n = e.axes.len() as u32;
        w(s, 28, n)?;
        if room < n {
            return Ok(ApiRet::Ret(DIERR_MOREDATA));
        }
        if flags & DIEP_AXES != 0 {
            let pa = s.mem.read_u32(p + 32)?;
            for (i, o) in e.axes.iter().enumerate() {
                s.mem.write_u32(pa + 4 * i as u32, describe(&d, eflags, *o))?;
            }
        }
        if flags & DIEP_DIRECTION != 0 {
            let want = eflags & DIEFF_COORDS;
            // The stored system when the caller accepts it, else a conversion.
            let (sys, vals): (u32, Vec<i32>) = if want & e.dir_flags != 0 || want == 0 {
                (e.dir_flags, e.direction.clone())
            } else {
                let polar = e.polar() as i32;
                if want & DIEFF_POLAR != 0 {
                    (DIEFF_POLAR, vec![polar, 0])
                } else if want & DIEFF_CARTESIAN != 0 {
                    let a = (polar as f64 / 100.0).to_radians();
                    let x = (a.sin() * 10000.0).round() as i32;
                    let y = (-a.cos() * 10000.0).round() as i32;
                    let v = e.axes.iter().map(|o| if *o == Obj::Axis(1) { y } else { x }).collect();
                    (DIEFF_CARTESIAN, v)
                } else {
                    (DIEFF_SPHERICAL, vec![(polar - 9000).rem_euclid(36000), 0])
                }
            };
            w(s, 4, (eflags & !DIEFF_COORDS) | sys)?;
            let pd = s.mem.read_u32(p + 36)?;
            for i in 0..n {
                s.mem
                    .write_u32(pd + 4 * i, vals.get(i as usize).copied().unwrap_or(0) as u32)?;
            }
        }
    }
    if flags & DIEP_ENVELOPE != 0 {
        let pe = s.mem.read_u32(p + 40)?;
        match e.envelope {
            Some(env) if pe != 0 => {
                for (i, v) in [20, env.attack_level, env.attack_us, env.fade_level, env.fade_us]
                    .iter()
                    .enumerate()
                {
                    s.mem.write_u32(pe + 4 * i as u32, *v)?;
                }
            }
            Some(_) => {}
            None => w(s, 40, 0)?,
        }
    }
    if flags & DIEP_TYPESPECIFICPARAMS != 0 {
        let room = s.mem.read_u32(p + 44)?;
        w(s, 44, e.params.len() as u32)?;
        if room < e.params.len() as u32 {
            hr = DIERR_MOREDATA;
        } else {
            let pt = s.mem.read_u32(p + 48)?;
            s.mem.write(pt, &e.params)?;
        }
    }
    if flags & DIEP_STARTDELAY != 0 && size >= 56 {
        w(s, 52, e.start_delay)?;
    }
    Ok(ApiRet::Ret(hr))
}

fn eff_start(s: &mut Session) -> ApiResult {
    let [_this, iterations, flags] = s.args::<3>()?;
    let id = this_effect(s)?;
    Ok(ApiRet::Ret(start(s, id, iterations, flags)))
}

fn eff_stop(s: &mut Session) -> ApiResult {
    let id = this_effect(s)?;
    if !exclusive(s, d_id(s, id)) {
        return Ok(ApiRet::Ret(DIERR_NOTEXCLUSIVEACQUIRED));
    }
    if effect_mut(s, id).playing.take().is_some() {
        send(s, Command::Stop { id });
    }
    Ok(ApiRet::Ret(DI_OK))
}

fn eff_get_status(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let id = this_effect(s)?;
    if p == 0 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    // Without exclusive access nothing of this device plays.
    let now = s.clock.now_ns();
    let playing = exclusive(s, d_id(s, id)) && effect(s, id).is_playing(now);
    s.mem.write_u32(p, if playing { DIEGES_PLAYING } else { 0 })?;
    Ok(ApiRet::Ret(DI_OK))
}

fn eff_download(s: &mut Session) -> ApiResult {
    let id = this_effect(s)?;
    if !exclusive(s, d_id(s, id)) {
        return Ok(ApiRet::Ret(DIERR_NOTEXCLUSIVEACQUIRED));
    }
    if effect(s, id).downloaded {
        return Ok(ApiRet::Ret(DI_NOEFFECT));
    }
    if !effect(s, id).complete() {
        return Ok(ApiRet::Ret(DIERR_INCOMPLETEEFFECT));
    }
    download(s, id);
    Ok(ApiRet::Ret(DI_OK))
}

fn eff_unload(s: &mut Session) -> ApiResult {
    let id = this_effect(s)?;
    let e = effect_mut(s, id);
    e.playing = None;
    if std::mem::replace(&mut e.downloaded, false) {
        send(s, Command::Unload { id });
    }
    Ok(ApiRet::Ret(DI_OK))
}

fn eff_escape(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DIERR_UNSUPPORTED))
}

pub static IDIRECTINPUTEFFECT: IfaceDef = IfaceDef {
    name: "IDirectInputEffect",
    iid: guid("e7e1f7c0-88d2-11d0-9ad0-00a0c9a06e35"),
    module: "DINPUT.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("Initialize", 4, eff_initialize),
        export!("GetEffectGuid", 2, eff_get_guid),
        export!("GetParameters", 3, eff_get_parameters),
        export!("SetParameters", 3, eff_set_parameters),
        export!("Start", 3, eff_start),
        export!("Stop", 1, eff_stop),
        export!("GetEffectStatus", 2, eff_get_status),
        export!("Download", 1, eff_download),
        export!("Unload", 1, eff_unload),
        export!("Escape", 2, eff_escape),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use drydock_core::mem::{PAGE_READWRITE, RegionKind};
    use std::sync::{Arc, Mutex};

    struct Record(Arc<Mutex<Vec<Command>>>);

    impl ff::ForceSink for Record {
        fn send(&mut self, _at_ns: u64, c: Command) {
            self.0.lock().unwrap().push(c);
        }
        fn name(&self) -> String {
            "record".into()
        }
    }

    /// A session with an exclusively acquired joystick in the DIJOYSTATE layout.
    struct T {
        s: Session,
        dev: u32,
        dev_ptr: u32,
        mem: u32,
        sent: Arc<Mutex<Vec<Command>>>,
    }

    const X: u32 = 0;
    const Y: u32 = 4;

    impl T {
        fn new() -> T {
            let mut s = crate::testutil::session_with(&drydock_fixtures::exit_with(0));
            let sent = Arc::new(Mutex::new(Vec::new()));
            s.host.ff = Some(Box::new(Record(sent.clone())));
            let mem = s
                .mem
                .alloc(None, 0x10000, RegionKind::Private, PAGE_READWRITE, "test")
                .unwrap();
            let mut d = dinput::new_device(DevKind::Joystick);
            let ofs = [
                (Obj::Axis(0), 0),
                (Obj::Axis(1), 4),
                (Obj::Axis(5), 20),
                (Obj::Axis(6), 24),
            ];
            d.format = ofs.iter().map(|&(obj, ofs)| dinput::FormatEntry { obj, ofs }).collect();
            d.format.extend((0..8).map(|b| dinput::FormatEntry {
                obj: Obj::Button(b),
                ofs: 48 + b,
            }));
            d.data_size = 80;
            d.coop = 1 | 4; // DISCL_EXCLUSIVE | DISCL_FOREGROUND
            d.acquired = true;
            let dev = s.com_new(ComKind::DInputDevice(Box::new(d)));
            let dev_ptr = s.iface(dev, &dinput::IDIRECTINPUTDEVICE7A).unwrap();
            T {
                s,
                dev,
                dev_ptr,
                mem,
                sent,
            }
        }

        /// Call a method with `this` and arguments on a scratch stack.
        fn call(&mut self, h: fn(&mut Session) -> ApiResult, args: &[u32]) -> u32 {
            let sp = self.mem + 0xf000;
            self.s.mem.write_u32(sp, 0).unwrap();
            for (i, a) in args.iter().enumerate() {
                self.s.mem.write_u32(sp + 4 + 4 * i as u32, *a).unwrap();
            }
            self.s.cpu.set_esp(sp);
            match h(&mut self.s).unwrap() {
                ApiRet::Ret(v) => v,
                _ => panic!("unexpected return"),
            }
        }

        fn sent(&self) -> Vec<Command> {
            std::mem::take(&mut *self.sent.lock().unwrap())
        }

        fn write(&mut self, at: u32, words: &[u32]) -> u32 {
            let p = self.mem + at;
            for (i, w) in words.iter().enumerate() {
                self.s.mem.write_u32(p + 4 * i as u32, *w).unwrap();
            }
            p
        }

        /// A DIEFFECT over X and Y (by offset) with cartesian direction `dir` and
        /// the given type-specific parameters.
        fn dieffect(&mut self, duration: u32, dir: [i32; 2], params: &[u32], env: Option<[u32; 4]>) -> u32 {
            let axes = self.write(0x100, &[X, Y]);
            let dirp = self.write(0x110, &[dir[0] as u32, dir[1] as u32]);
            let tp = self.write(0x120, params);
            let envp = match env {
                Some(e) => self.write(0x180, &[20, e[0], e[1], e[2], e[3]]),
                None => 0,
            };
            self.write(
                0x200,
                &[
                    56,
                    DIEFF_OBJECTOFFSETS | DIEFF_CARTESIAN,
                    duration,
                    0,
                    10000,
                    DIEB_NOTRIGGER,
                    0,
                    2,
                    axes,
                    dirp,
                    envp,
                    params.len() as u32 * 4,
                    tp,
                    0,
                ],
            )
        }

        fn create(&mut self, g: Guid, peff: u32) -> (u32, u32) {
            let gp = self.mem + 0x300;
            self.s.mem.write(gp, &g).unwrap();
            let out = self.mem + 0x320;
            let hr = self.call(dev_create_effect, &[self.dev_ptr, gp, peff, out, 0]);
            let ptr = self.s.mem.read_u32(out).unwrap();
            (hr, ptr)
        }

        fn status(&mut self, eff: u32) -> u32 {
            let out = self.mem + 0x330;
            assert_eq!(self.call(eff_get_status, &[eff, out]), DI_OK);
            self.s.mem.read_u32(out).unwrap()
        }
    }

    const CONSTANT: Guid = EFFECTS[0].0;
    const SINE: Guid = EFFECTS[3].0;

    #[test]
    fn constant_force_downloads_plays_and_finishes_on_virtual_time() {
        let mut t = T::new();
        let p = t.dieffect(250_000, [1, 0], &[(-7500i32) as u32], Some([0, 0, 0, 41_666]));
        let (hr, eff) = t.create(CONSTANT, p);
        assert_eq!(hr, DI_OK);
        let id = t.s.obj_of(eff).unwrap();
        let sent = t.sent();
        let [Command::Download { id: did, effect }] = &sent[..] else {
            panic!("{sent:?}")
        };
        assert_eq!(*did, id);
        assert_eq!(effect.kind, EffectKind::Constant { magnitude: -7500 });
        assert_eq!(effect.direction, 9000, "east");
        assert_eq!(effect.axes, [ff::Axis::X, ff::Axis::Y]);
        assert_eq!(effect.envelope.unwrap().fade_us, 41_666);
        assert_eq!(t.status(eff), 0);
        assert_eq!(t.call(eff_start, &[eff, 2, 0]), DI_OK);
        assert_eq!(
            t.sent(),
            [Command::Start {
                id,
                iterations: 2,
                solo: false
            }]
        );
        assert_eq!(t.status(eff), DIEGES_PLAYING);
        let start = t.s.clock.now_ns();
        t.s.clock.advance_to(start + 499_000_000);
        assert_eq!(t.status(eff), DIEGES_PLAYING, "two 250 ms iterations");
        t.s.clock.advance_to(start + 500_000_000);
        assert_eq!(t.status(eff), 0);
    }

    #[test]
    fn effects_need_exclusive_acquisition() {
        let mut t = T::new();
        match &mut t.s.com_obj_mut(t.dev).unwrap().kind {
            ComKind::DInputDevice(d) => d.coop = 2 | 4, // non-exclusive
            _ => unreachable!(),
        }
        let p = t.dieffect(INFINITE, [0, 1], &[5000], None);
        let (hr, eff) = t.create(CONSTANT, p);
        assert_eq!(hr, DI_OK, "created, download skipped");
        assert_eq!(t.call(eff_set_parameters, &[eff, p, DIEP_GAIN]), DI_DOWNLOADSKIPPED);
        assert_eq!(t.call(eff_start, &[eff, 1, 0]), DIERR_NOTEXCLUSIVEACQUIRED);
        assert!(t.sent().is_empty());
    }

    #[test]
    fn unacquire_unloads_and_release_frees() {
        let mut t = T::new();
        let p = t.dieffect(INFINITE, [0, -1], &[4400, 0, 0, 90_900], None);
        let (_, eff) = t.create(SINE, p);
        let id = t.s.obj_of(eff).unwrap();
        t.call(eff_start, &[eff, INFINITE, 0]);
        t.sent();
        let dev_ptr = t.dev_ptr;
        assert_eq!(t.call(dinput::dev_unacquire, &[dev_ptr]), DI_OK);
        assert_eq!(t.sent(), [Command::Unload { id }]);
        assert_eq!(effect(&t.s, id).playing, None);
        // Releasing a downloaded effect unloads it and drops its device reference.
        let refs = t.s.com_obj(t.dev).unwrap().refs;
        if let ComKind::DInputDevice(d) = &mut t.s.com_obj_mut(t.dev).unwrap().kind {
            d.acquired = true;
        }
        t.call(eff_download, &[eff]);
        t.sent();
        t.s.com_release(id);
        assert_eq!(t.sent(), [Command::Unload { id }]);
        assert_eq!(t.s.com_obj(t.dev).unwrap().refs, refs - 1);
    }

    #[test]
    fn get_parameters_converts_direction_and_reports_sizes() {
        let mut t = T::new();
        let p = t.dieffect(INFINITE, [0, 1], &[1000], None);
        let (_, eff) = t.create(CONSTANT, p);
        // Ask for polar coordinates, by id, with room for one axis only first.
        let q = t.write(
            0x400,
            &[
                56,
                DIEFF_POLAR,
                0,
                0,
                0,
                0,
                0,
                1,
                t.mem + 0x480,
                t.mem + 0x490,
                0,
                0,
                0,
                0,
            ],
        );
        assert_eq!(t.call(eff_get_parameters, &[eff, q, DIEP_AXES]), DIERR_MOREDATA);
        assert_eq!(t.s.mem.read_u32(q + 28).unwrap(), 2);
        let flags = DIEP_AXES | DIEP_DIRECTION | DIEP_TYPESPECIFICPARAMS;
        assert_eq!(
            t.call(eff_get_parameters, &[eff, q, flags]),
            DIERR_MOREDATA,
            "no params room"
        );
        assert_eq!(t.s.mem.read_u32(q + 44).unwrap(), 4);
        assert_eq!(t.s.mem.read_u32(q + 4).unwrap(), DIEFF_POLAR);
        assert_eq!(t.s.mem.read_i32(t.mem + 0x490).unwrap(), 18000, "south");
        let (x, y) = (
            t.s.mem.read_u32(t.mem + 0x480).unwrap(),
            t.s.mem.read_u32(t.mem + 0x484).unwrap(),
        );
        assert_eq!(
            (x & 0xffff, y & 0xffff),
            (0x0002, 0x0102),
            "X and Y absolute axes, instances 0 and 1"
        );
        assert_ne!(x & 0x0100_0000, 0, "force-feedback actuator");
    }

    #[test]
    fn resync_replays_downloaded_and_playing_effects() {
        let mut t = T::new();
        let p = t.dieffect(INFINITE, [1, 0], &[3000], None);
        let (_, a) = t.create(CONSTANT, p);
        let (_, b) = t.create(CONSTANT, p);
        t.call(eff_start, &[b, 1, 0]);
        t.call(eff_unload, &[a]);
        t.sent();
        let b = t.s.obj_of(b).unwrap();
        resync(&mut t.s);
        let sent = t.sent();
        assert_eq!(
            sent[..3],
            [
                Command::Gain(10000),
                Command::Autocenter(true),
                Command::Actuators(true)
            ]
        );
        assert!(matches!(sent[3], Command::Download { id, .. } if id == b));
        assert_eq!(
            sent[4],
            Command::Start {
                id: b,
                iterations: 1,
                solo: false
            }
        );
        assert_eq!(sent.len(), 5, "the unloaded effect stays off the device");
    }

    #[test]
    fn device_commands_and_properties_reach_the_host() {
        let mut t = T::new();
        let dev = t.dev_ptr;
        assert_eq!(
            t.call(dev_send_ff_command, &[dev, DISFFC_RESET | DISFFC_SETACTUATORSON]),
            DI_OK
        );
        assert_eq!(t.sent(), [Command::Reset, Command::Actuators(true)]);
        let out = t.mem + 0x500;
        assert_eq!(t.call(dev_get_ff_state, &[dev, out]), DI_OK);
        let st = t.s.mem.read_u32(out).unwrap();
        assert_eq!(st & (DIGFFS_EMPTY | DIGFFS_STOPPED | DIGFFS_ACTUATORSON), 0x13);
        // DIPROPDWORD header: size 20, header 16, object 0, DIPH_DEVICE, value.
        let ph = t.write(0x600, &[20, 16, 0, 0, 0]);
        assert_eq!(t.call(dinput::dev_set_property, &[dev, 9, ph]), DI_OK);
        t.write(0x610, &[3000]);
        assert_eq!(t.call(dinput::dev_set_property, &[dev, 7, ph]), DI_OK);
        assert_eq!(t.sent(), [Command::Autocenter(false), Command::Gain(3000)]);
    }
}
