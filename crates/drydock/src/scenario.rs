//! Declarative scenarios: ordinary input, bounded waits, captures and assertions over
//! exposed observations. The executor knows nothing about any particular program;
//! program-specific scenarios are external local data.
//!
//! ```json
//! { "name": "example", "steps": [
//!     { "run": 2.0 },
//!     { "wait_for": { "window_title": "Setup" }, "timeout": 10 },
//!     { "key": "ENTER" },
//!     { "key": "A", "mods": ["ALT"] },
//!     { "click": [320, 240] },
//!     { "screenshot": "after.png" },
//!     { "assert": { "frames_at_least": 1 } },
//!     { "savepoint": "before-race" },
//!     { "save": "before-race.ddck" },
//!     { "restore": "before-race" }
//! ] }
//! ```
//!
//! `savepoint` keeps a checkpoint in memory for this run; `save` writes one to the
//! artifacts directory. `restore` takes a savepoint name, or a checkpoint file (relative
//! to the artifacts directory).
//!
//! ```text
//! ```

use crate::driver::Driver;
use anyhow::{Result, bail};
use drydock_win32::checkpoint::Checkpoint;
use drydock_win32::input::{InputEvent, parse_vk};
use drydock_win32::session::RunStop;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    #[serde(default)]
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Step {
    Run {
        run: f64,
    },
    Key {
        key: String,
        #[serde(default)]
        mods: Vec<String>,
        #[serde(default)]
        hold: Option<f64>,
    },
    KeyDown {
        key_down: String,
    },
    KeyUp {
        key_up: String,
    },
    Type {
        r#type: String,
    },
    Click {
        click: [i32; 2],
        #[serde(default)]
        button: Option<String>,
    },
    MouseMove {
        mouse_move: [i32; 2],
    },
    MouseRel {
        mouse_rel: [i32; 2],
    },
    Screenshot {
        screenshot: String,
    },
    WaitFor {
        wait_for: Cond,
        #[serde(default = "default_timeout")]
        timeout: f64,
    },
    Assert {
        assert: Cond,
    },
    AnswerDialog {
        answer_dialog: u32,
    },
    ReleaseAll {
        release_all: bool,
    },
    Savepoint {
        savepoint: String,
    },
    Save {
        save: String,
    },
    Restore {
        restore: String,
    },
    Note {
        note: String,
    },
}

fn default_timeout() -> f64 {
    10.0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cond {
    pub window_title: Option<String>,
    pub window_class: Option<String>,
    pub dialog_open: Option<bool>,
    pub api_called: Option<String>,
    pub frames_at_least: Option<u64>,
    pub debug_output_contains: Option<String>,
    pub module_loaded: Option<String>,
    pub exited: Option<bool>,
    pub display_mode: Option<[u32; 3]>,
}

impl Cond {
    pub fn check(&self, d: &Driver) -> bool {
        let s = &d.s;
        if let Some(t) = &self.window_title
            && !s
                .user
                .windows
                .values()
                .any(|w| w.visible && w.title.contains(t.as_str()))
        {
            return false;
        }
        if let Some(c) = &self.window_class
            && !s.user.windows.values().any(|w| w.class.eq_ignore_ascii_case(c))
        {
            return false;
        }
        if let Some(open) = self.dialog_open
            && open != s.user.dialogs.iter().any(|d| d.answer.is_none())
            && open != !s.user.dialogs_mgr.is_empty()
        {
            return false;
        }
        if let Some(a) = &self.api_called
            && !s.diag.api_counts.keys().any(|k| k.eq_ignore_ascii_case(a))
        {
            return false;
        }
        if let Some(n) = self.frames_at_least
            && s.frames_presented < n
        {
            return false;
        }
        if let Some(t) = &self.debug_output_contains
            && !s.diag.debug_output.iter().any(|l| l.contains(t.as_str()))
        {
            return false;
        }
        if let Some(m) = &self.module_loaded
            && s.modules.by_name(m).is_none()
        {
            return false;
        }
        if let Some(e) = self.exited
            && e != s.exited.is_some()
        {
            return false;
        }
        if let Some([w, h, b]) = self.display_mode {
            let m = s.user.mode;
            if (m.width, m.height, m.bpp) != (w, h, b) {
                return false;
            }
        }
        true
    }
}

pub struct Outcome {
    pub ok: bool,
    pub log: Vec<serde_json::Value>,
    pub stop: Option<RunStop>,
}

fn secs(x: f64) -> u64 {
    (x * 1e9) as u64
}

fn vk(name: &str) -> Result<u8> {
    parse_vk(name).ok_or_else(|| anyhow::anyhow!("unknown key {name:?}"))
}

/// Advance and report terminal stops (exit/halt) as errors for the scenario.
fn advance(d: &mut Driver, dt: u64) -> Option<RunStop> {
    match d.advance(dt, false) {
        RunStop::TimeReached => None,
        other => Some(other),
    }
}

/// State that persists across the steps of one run (or one protocol session).
#[derive(Default)]
pub struct Ctx {
    pub savepoints: std::collections::HashMap<String, Checkpoint>,
}

pub enum StepOutcome {
    Continue,
    Failed(String),
    Stopped(RunStop),
}

/// Execute one step. Observations it produces are appended to `log`.
pub fn step(
    d: &mut Driver,
    ctx: &mut Ctx,
    step: &Step,
    artifacts: &Path,
    log: &mut Vec<serde_json::Value>,
) -> Result<StepOutcome> {
    let stop = match step {
        Step::Run { run } => advance(d, secs(*run)),
        Step::Key { key, mods, hold } => {
            let now = d.s.clock.now_ns();
            let hold = secs(hold.unwrap_or(0.06));
            let mut mvks = Vec::new();
            for m in mods {
                let v = vk(m)?;
                d.s.key(v, true, now);
                mvks.push(v);
            }
            let k = vk(key)?;
            d.s.tap_key(k, now + 10_000_000, hold);
            for v in mvks.iter().rev() {
                d.s.key(*v, false, now + 20_000_000 + hold);
            }
            advance(d, hold + 60_000_000)
        }
        Step::KeyDown { key_down } => {
            let now = d.s.clock.now_ns();
            d.s.key(vk(key_down)?, true, now);
            advance(d, 1_000_000)
        }
        Step::KeyUp { key_up } => {
            let now = d.s.clock.now_ns();
            d.s.key(vk(key_up)?, false, now);
            advance(d, 1_000_000)
        }
        Step::Type { r#type } => {
            let mut now = d.s.clock.now_ns();
            for c in r#type.chars() {
                let up = c.to_ascii_uppercase();
                let shift = c.is_ascii_uppercase() || "!@#$%^&*()_+{}|:\"<>?~".contains(c);
                let k = match up {
                    ' ' => 0x20,
                    '\n' => 0x0d,
                    c if c.is_ascii_alphanumeric() => c as u8,
                    _ => bail!("cannot type {c:?}"),
                };
                if shift {
                    d.s.key(0xa0, true, now);
                }
                d.s.tap_key(k, now + 5_000_000, 40_000_000);
                if shift {
                    d.s.key(0xa0, false, now + 50_000_000);
                }
                now += 80_000_000;
            }
            let dt = now - d.s.clock.now_ns() + 20_000_000;
            advance(d, dt)
        }
        Step::Click { click, button } => {
            let b = match button.as_deref() {
                Some("right") => 1,
                Some("middle") => 2,
                _ => 0,
            };
            let now = d.s.clock.now_ns();
            let inp = &mut d.s.user.input;
            inp.schedule(
                now,
                InputEvent::MouseMove {
                    x: click[0],
                    y: click[1],
                },
            );
            inp.schedule(now + 10_000_000, InputEvent::MouseButton { button: b, down: true });
            inp.schedule(now + 70_000_000, InputEvent::MouseButton { button: b, down: false });
            advance(d, 120_000_000)
        }
        Step::MouseMove { mouse_move } => {
            let now = d.s.clock.now_ns();
            d.s.user.input.schedule(
                now,
                InputEvent::MouseMove {
                    x: mouse_move[0],
                    y: mouse_move[1],
                },
            );
            advance(d, 10_000_000)
        }
        Step::MouseRel { mouse_rel } => {
            let now = d.s.clock.now_ns();
            d.s.user.input.schedule(
                now,
                InputEvent::MouseRel {
                    dx: mouse_rel[0],
                    dy: mouse_rel[1],
                },
            );
            advance(d, 10_000_000)
        }
        Step::Screenshot { screenshot } => {
            let path = artifacts.join(screenshot);
            if let Some((w, h, px)) = d.s.capture_front() {
                std::fs::write(&path, drydock_win32::observe::encode_png(w, h, &px))?;
                log.push(serde_json::json!({ "screenshot": path, "size": [w, h] }));
            }
            None
        }
        Step::WaitFor { wait_for, timeout } => {
            let deadline = d.s.clock.now_ns() + secs(*timeout);
            let mut stop = None;
            loop {
                if wait_for.check(d) {
                    break;
                }
                if d.s.clock.now_ns() >= deadline {
                    return Ok(StepOutcome::Failed("wait_for timed out".into()));
                }
                if let Some(s) = advance(d, 10_000_000) {
                    stop = Some(s);
                    break;
                }
            }
            stop
        }
        Step::Assert { assert } => {
            if !assert.check(d) {
                return Ok(StepOutcome::Failed("assertion failed".into()));
            }
            None
        }
        Step::AnswerDialog { answer_dialog } => {
            if !d.s.answer_dialog(*answer_dialog) {
                return Ok(StepOutcome::Failed("no pending message box".into()));
            }
            advance(d, 1_000_000)
        }
        Step::ReleaseAll { .. } => {
            let now = d.s.clock.now_ns();
            d.s.user.input.release_all(now);
            advance(d, 1_000_000)
        }
        Step::Savepoint { savepoint } => {
            let t0 = std::time::Instant::now();
            ctx.savepoints.insert(savepoint.clone(), d.s.checkpoint(savepoint));
            log.push(serde_json::json!({ "savepoint": savepoint, "host_ms": t0.elapsed().as_secs_f64() * 1e3 }));
            None
        }
        Step::Save { save } => {
            let t0 = std::time::Instant::now();
            let path = artifacts.join(save);
            let cp = d.s.checkpoint(save);
            cp.write_to(&path).map_err(anyhow::Error::msg)?;
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            log.push(serde_json::json!({ "saved": path, "bytes": bytes, "host_ms": t0.elapsed().as_secs_f64() * 1e3 }));
            None
        }
        Step::Restore { restore } => {
            let t0 = std::time::Instant::now();
            let cp = match ctx.savepoints.get(restore) {
                Some(cp) => cp.clone(),
                None => Checkpoint::read_from(&artifacts.join(restore)).map_err(anyhow::Error::msg)?,
            };
            d.s.restore(&cp, true).map_err(anyhow::Error::msg)?;
            d.reanchor();
            log.push(serde_json::json!({ "restored": restore, "t": cp.info.virtual_ns as f64 / 1e9, "host_ms": t0.elapsed().as_secs_f64() * 1e3 }));
            None
        }
        Step::Note { .. } => None,
    };
    Ok(match stop {
        Some(s) => StepOutcome::Stopped(s),
        None => StepOutcome::Continue,
    })
}

pub fn run(d: &mut Driver, sc: &Scenario, artifacts: &Path) -> Result<Outcome> {
    let mut log = Vec::new();
    let mut ctx = Ctx::default();
    for (i, st) in sc.steps.iter().enumerate() {
        let t = d.s.clock.now_ns() as f64 / 1e9;
        let wall = d.wall_start.elapsed().as_secs_f64();
        log.push(serde_json::json!({ "step": i, "t": t, "wall": wall, "do": st }));
        match step(d, &mut ctx, st, artifacts, &mut log)? {
            StepOutcome::Continue => {}
            StepOutcome::Failed(reason) => {
                log.push(serde_json::json!({ "failed": i, "reason": reason }));
                return Ok(Outcome {
                    ok: false,
                    log,
                    stop: None,
                });
            }
            StepOutcome::Stopped(stop) => {
                let ok = matches!(stop, RunStop::Exited { .. }) && i + 1 == sc.steps.len();
                log.push(serde_json::json!({ "stopped": stop, "at_step": i }));
                return Ok(Outcome {
                    ok,
                    log,
                    stop: Some(stop),
                });
            }
        }
    }
    Ok(Outcome {
        ok: true,
        log,
        stop: None,
    })
}
