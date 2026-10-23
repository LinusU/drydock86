//! `drydock joystick`: check a host game controller before playing with it — what
//! Drydock finds in its report descriptor, live input, and test force-feedback effects.

use anyhow::{Context, Result, anyhow, bail};
use drydock_joystick::device::{self, Controller};
use drydock_joystick::ff::{Axis, Command, Condition, ConditionKind, Effect, EffectKind, Envelope, Waveform};
use drydock_joystick::hid::ReportKind;
use drydock_joystick::input::{JoyState, POV_CENTERED};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Ctrl-C: end the test so the controller is reset on the way out.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

/// Wait while reading input (the controller counts the reports), stopping early when
/// interrupted. Returns whether to carry on.
fn pause(c: &mut Controller, ms: u64) -> Result<bool> {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut st = JoyState::default();
    while Instant::now() < end && !interrupted() {
        c.read(&mut st, 10).map_err(|e| anyhow!(e))?;
    }
    Ok(!interrupted())
}

#[derive(clap::Args)]
pub struct JoystickArgs {
    /// Only list the game controllers.
    #[arg(long)]
    pub list: bool,
    /// Controller to use: "045e:001b" (vendor:product) or part of its name.
    #[arg(long)]
    pub device: Option<String>,
    /// Seconds of live input to show.
    #[arg(long, default_value_t = 10.0)]
    pub seconds: f64,
    /// Play a sequence of test effects (pushes the stick; hold it loosely).
    #[arg(long)]
    pub test_force_feedback: bool,
    /// Strength of the test effects, in percent.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(0..=100))]
    pub strength: u32,
    /// Write the controller's raw HID report descriptor to this file.
    #[arg(long)]
    pub save_descriptor: Option<std::path::PathBuf>,
}

pub fn run(a: JoystickArgs) -> Result<()> {
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::Relaxed));
    let api = device::api().map_err(|e| anyhow!(e))?;
    let found = device::candidates(&api);
    if a.list || found.is_empty() {
        if found.is_empty() {
            println!("No game controllers found.");
        }
        for c in &found {
            println!("{c}");
        }
        return Ok(());
    }
    let c = match &a.device {
        Some(f) => found
            .iter()
            .find(|c| c.matches(f))
            .with_context(|| format!("no controller matches {f:?}"))?,
        None => &found[0],
    };
    let ff = a.test_force_feedback.then_some(a.strength);
    let mut ctl = Controller::open(&api, c, ff).map_err(|e| anyhow!(e))?;
    describe(&ctl);
    if let Some(p) = &a.save_descriptor {
        std::fs::write(p, &ctl.descriptor)?;
        println!("report descriptor written to {}", p.display());
    }
    if a.test_force_feedback {
        if !ctl.has_force_feedback() {
            bail!(
                "{} has no usable force feedback{}",
                ctl.info,
                ctl.ff_error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
            );
        }
        test_effects(&mut ctl)?;
    }
    live_input(&mut ctl, a.seconds)
}

fn describe(c: &Controller) {
    println!("{}", c.info);
    println!(
        "  {} axes, {} buttons, {}",
        c.layout.axis_count(),
        c.layout.button_count(),
        if c.layout.has_hat() { "a hat" } else { "no hat" }
    );
    for kind in [ReportKind::Input, ReportKind::Output, ReportKind::Feature] {
        let ids = c.desc.report_ids(kind);
        if !ids.is_empty() {
            println!("  {kind:?} reports: {ids:?}");
        }
    }
    let pid = drydock_joystick::pid::supports(&c.desc);
    println!(
        "  force feedback: {}",
        match (pid, c.has_force_feedback(), &c.ff_error) {
            (false, _, _) => "none (no USB PID reports)".to_string(),
            (true, true, _) => format!("USB PID, working {:?}", c.ff_info()),
            (true, false, Some(e)) => format!("USB PID, failed: {e}"),
            (true, false, None) => "USB PID (not opened; use --test-force-feedback)".to_string(),
        }
    );
}

fn live_input(c: &mut Controller, seconds: f64) -> Result<()> {
    if seconds <= 0.0 {
        return Ok(());
    }
    println!("Move the stick, twist it, slide the throttle, press buttons ({seconds} s)…");
    let mut st = JoyState::default();
    c.current(&mut st);
    let mut shown: Option<JoyState> = None;
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end && !interrupted() {
        c.read(&mut st, 20).map_err(|e| anyhow!(e))?;
        if shown != Some(st) {
            let [x, y, throttle, twist] = st.axes;
            let pov = if st.pov == POV_CENTERED {
                "centred".to_string()
            } else {
                format!("{}°", st.pov / 100)
            };
            println!(
                "  x {x:5}  y {y:5}  twist {twist:5}  throttle {throttle:5}  hat {pov:>7}  buttons 1-8 {:08b}",
                st.buttons.reverse_bits() >> 24
            );
            shown = Some(st);
        }
    }
    Ok(())
}

fn effect(kind: EffectKind, duration_us: u32, direction: u32) -> Effect {
    Effect {
        kind,
        duration_us,
        sample_period_us: 0,
        gain: 10000,
        trigger_button: None,
        trigger_repeat_us: 0,
        axes: vec![Axis::X, Axis::Y],
        direction,
        envelope: None,
        start_delay_us: 0,
    }
}

fn test_effects(c: &mut Controller) -> Result<()> {
    let spring = Condition {
        offset: 0,
        positive_coefficient: 10000,
        negative_coefficient: 10000,
        positive_saturation: 10000,
        negative_saturation: 10000,
        dead_band: 0,
    };
    let mut gun = effect(
        EffectKind::Periodic {
            waveform: Waveform::Square,
            magnitude: 5600,
            offset: 0,
            phase: 0,
            period_us: 125_000,
        },
        125_000,
        0,
    );
    gun.envelope = Some(Envelope {
        attack_level: 5600,
        attack_us: 0,
        fade_level: 0,
        fade_us: 60_000,
    });
    let steps: Vec<(&str, Effect, u32, u64)> = vec![
        (
            "push to the right",
            effect(EffectKind::Constant { magnitude: 6000 }, 1_000_000, 9000),
            1,
            1500,
        ),
        (
            "push towards you",
            effect(EffectKind::Constant { magnitude: 6000 }, 1_000_000, 0),
            1,
            1500,
        ),
        (
            "sine rumble",
            effect(
                EffectKind::Periodic {
                    waveform: Waveform::Sine,
                    magnitude: 5000,
                    offset: 0,
                    phase: 0,
                    period_us: 80_000,
                },
                1_500_000,
                9000,
            ),
            1,
            2000,
        ),
        ("machine gun (8 shots)", gun, 8, 1500),
        (
            "stiff spring (move the stick)",
            effect(
                EffectKind::Condition {
                    kind: ConditionKind::Spring,
                    axes: vec![spring, spring],
                },
                3_000_000,
                0,
            ),
            1,
            3500,
        ),
    ];
    let force = |c: &mut Controller, cmd| c.force(cmd).map_err(|e| anyhow!("force feedback: {e}"));
    force(c, Command::Autocenter(false))?;
    println!("Force feedback test (the auto-centre spring is off between steps):");
    for (n, (name, e, iterations, wait_ms)) in steps.into_iter().enumerate() {
        println!("  {name}");
        let id = n as u32 + 1;
        force(c, Command::Download { id, effect: e })?;
        force(
            c,
            Command::Start {
                id,
                iterations,
                solo: false,
            },
        )?;
        let go_on = pause(c, wait_ms)?;
        force(c, Command::Stop { id })?;
        force(c, Command::Unload { id })?;
        if !go_on || !pause(c, 300)? {
            break;
        }
    }
    force(c, Command::Autocenter(true))?;
    println!("  auto-centre spring back on");
    let s = c.stats;
    println!(
        "  the stick confirmed {} effect starts ({} state reports) and sent {} control reports",
        s.playing, s.effect_states, s.controls
    );
    Ok(())
}
