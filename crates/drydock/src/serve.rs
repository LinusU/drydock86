//! `drydock serve --stdio`: a line-delimited JSON protocol for driving sessions.
//!
//! Each request is one JSON object per line: `{"id": 1, "method": "run", "params": {...}}`.
//! Each response echoes the id: `{"id": 1, "result": ...}` or
//! `{"id": 1, "error": {"code": "...", "message": "..."}}`. Requests are handled in
//! order, except `cancel`, which is answered immediately and interrupts a running
//! `run` (or a long `step`). See `docs/protocol.md` for the method reference.

use crate::driver::{Driver, Pacing};
use crate::launch::{self, RunArgs};
use crate::scenario::{self, Ctx, Step, StepOutcome};
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, FromArgMatches};
use drydock_win32::checkpoint::Checkpoint;
use drydock_win32::input::InputEvent;
use drydock_win32::session::{RunLimit, RunStop};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

pub const PROTOCOL_VERSION: u32 = 1;

const METHODS: &[&str] = &[
    "hello",
    "open",
    "close",
    "run",
    "cancel",
    "step",
    "scenario",
    "input",
    "input.recording",
    "screenshot",
    "state",
    "events",
    "report",
    "memory.read",
    "memory.write",
    "memory.search",
    "regs.get",
    "regs.set",
    "breakpoint.add",
    "breakpoint.remove",
    "watchpoint.add",
    "api_break.add",
    "api_break.remove",
    "trace.set",
    "trace.get",
    "savepoint.create",
    "savepoint.restore",
    "savepoint.list",
    "savepoint.delete",
    "checkpoint.save",
    "checkpoint.load",
    "shutdown",
];

struct Open {
    d: Driver,
    args: RunArgs,
    ctx: Ctx,
    artifacts: PathBuf,
}

struct Server {
    open: Option<Open>,
    cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, v: &Value) {
    let mut o = out.lock().unwrap();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// Convert `{"exe": "x", "overlay": "o", "joystick": true, "reg": ["a", "b"]}` into
/// command-line arguments for [`RunArgs`], so the CLI and the protocol share one
/// definition of session options.
fn run_args_from_json(p: &Value) -> Result<RunArgs> {
    let mut argv = vec!["open".to_string()];
    let obj = p.as_object().ok_or_else(|| anyhow!("params must be an object"))?;
    for (k, v) in obj {
        let flag = format!("--{}", k.replace('_', "-"));
        match v {
            Value::Bool(true) => argv.push(flag),
            Value::Bool(false) | Value::Null => {}
            Value::Array(items) => {
                for i in items {
                    argv.push(flag.clone());
                    argv.push(scalar(i)?);
                }
            }
            other => {
                argv.push(flag);
                argv.push(scalar(other)?);
            }
        }
    }
    let cmd = RunArgs::augment_args(clap::Command::new("open").no_binary_name(false));
    let m = cmd
        .try_get_matches_from(argv)
        .map_err(|e| anyhow!("{}", e.to_string().trim()))?;
    Ok(RunArgs::from_arg_matches(&m)?)
}

fn scalar(v: &Value) -> Result<String> {
    Ok(match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => bail!("unsupported option value {v}"),
    })
}

fn u32_param(p: &Value, k: &str) -> Result<u32> {
    let v = &p[k];
    if let Some(n) = v.as_u64() {
        return Ok(n as u32);
    }
    if let Some(s) = v.as_str() {
        let s = s.trim();
        let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(h) => u32::from_str_radix(h, 16),
            None => s.parse(),
        };
        return r.with_context(|| format!("bad {k}: {s:?}"));
    }
    bail!("missing parameter {k}")
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        bail!("odd-length hex string");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

impl Server {
    fn session(&mut self) -> Result<&mut Open> {
        self.open
            .as_mut()
            .ok_or_else(|| anyhow!("no session is open (call \"open\" first)"))
    }

    fn handle(&mut self, method: &str, p: &Value) -> Result<Value> {
        match method {
            "hello" => Ok(json!({
                "protocol": PROTOCOL_VERSION,
                "drydock_version": env!("CARGO_PKG_VERSION"),
                "methods": METHODS,
            })),
            "open" => self.open_session(p),
            "close" => {
                if let Some(o) = &self.open
                    && !o.args.no_persist
                {
                    o.d.s.persist().context("writing the overlay")?;
                }
                self.open = None;
                *self.cancel.lock().unwrap() = None;
                Ok(json!({}))
            }
            "run" => self.run(p),
            "step" => {
                let st: Step = serde_json::from_value(p["step"].clone()).context("bad step")?;
                let o = self.session()?;
                let mut log = Vec::new();
                let r = scenario::step(&mut o.d, &mut o.ctx, &st, &o.artifacts, &mut log)?;
                Ok(json!({
                    "outcome": match r {
                        StepOutcome::Continue => json!("ok"),
                        StepOutcome::Failed(reason) => json!({ "failed": reason }),
                        StepOutcome::Stopped(stop) => json!({ "stopped": stop }),
                    },
                    "log": log,
                    "virtual_s": o.d.s.clock.now_ns() as f64 / 1e9,
                }))
            }
            "scenario" => {
                let sc: scenario::Scenario = match p.get("path").and_then(|v| v.as_str()) {
                    Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
                    None => serde_json::from_value(p["scenario"].clone()).context("bad scenario")?,
                };
                let o = self.session()?;
                let out = scenario::run(&mut o.d, &sc, &o.artifacts)?;
                Ok(json!({ "ok": out.ok, "stop": out.stop, "log": out.log }))
            }
            "input" => {
                let o = self.session()?;
                let now = o.d.s.clock.now_ns();
                let evs: Vec<Value> = p["events"].as_array().cloned().unwrap_or_default();
                let mut n = 0;
                for e in evs {
                    // `at` is an offset in seconds from now (default 0).
                    let at = now + (e.get("at").and_then(|v| v.as_f64()).unwrap_or(0.0) * 1e9) as u64;
                    let ev: InputEvent = serde_json::from_value(e.get("event").cloned().unwrap_or(e.clone()))
                        .context("bad input event")?;
                    o.d.s.user.input.schedule(at, ev);
                    n += 1;
                }
                Ok(json!({ "scheduled": n }))
            }
            "input.recording" => {
                let o = self.session()?;
                Ok(json!({ "events": o.d.s.user.input.recorded }))
            }
            "screenshot" => {
                let o = self.session()?;
                let (w, h, px) = o.d.s.capture_front().ok_or_else(|| anyhow!("nothing is displayed"))?;
                let png = drydock_win32::observe::encode_png(w, h, &px);
                match p.get("path").and_then(|v| v.as_str()) {
                    Some(path) => {
                        std::fs::write(path, &png)?;
                        Ok(json!({ "path": path, "width": w, "height": h }))
                    }
                    None => Ok(json!({ "width": w, "height": h, "png_base64": base64(&png) })),
                }
            }
            "state" => {
                let o = self.session()?;
                let s = &o.d.s;
                Ok(json!({
                    "virtual_s": s.clock.now_ns() as f64 / 1e9,
                    "insns": s.cpu.state().insns,
                    "frames_presented": s.frames_presented,
                    "display_mode": s.user.mode,
                    "exited": s.exited,
                    "halt": s.diag.halt,
                    "windows": s.windows_info(),
                    "threads": s.threads_info_static(),
                    "modules": s.modules.list.values().map(|m| json!({
                        "name": m.name, "base": m.base, "size": m.size, "builtin": m.builtin, "sha256": m.sha256,
                    })).collect::<Vec<_>>(),
                    "audio": s.audio_info(),
                    "missing": s.diag.missing,
                    "dialogs_open": s.user.dialogs.iter().filter(|d| d.answer.is_none()).count(),
                }))
            }
            "events" => {
                let o = self.session()?;
                let since = (p.get("since_s").and_then(|v| v.as_f64()).unwrap_or(0.0) * 1e9) as u64;
                let ev: Vec<_> = o.d.s.diag.events.iter().filter(|e| e.t_ns >= since).collect();
                Ok(json!({ "events": ev, "debug_output": o.d.s.diag.debug_output }))
            }
            "report" => {
                let o = self.session()?;
                let mut r = launch::report(&o.d, &RunStop::TimeReached);
                r.as_object_mut().unwrap().remove("stop");
                r["provenance"] = launch::provenance(&o.d.s, &o.args);
                Ok(r)
            }
            "memory.read" => {
                let addr = u32_param(p, "addr")?;
                let len = u32_param(p, "len")?.min(1 << 20);
                let o = self.session()?;
                let b = o.d.s.mem.read_vec(addr, len).map_err(|e| anyhow!("{e:?}"))?;
                Ok(json!({ "addr": addr, "hex": hex_encode(&b) }))
            }
            "memory.write" => {
                let addr = u32_param(p, "addr")?;
                let data = hex_decode(p["hex"].as_str().ok_or_else(|| anyhow!("missing hex"))?)?;
                let o = self.session()?;
                o.d.s.mem.write_force(addr, &data).map_err(|e| anyhow!("{e:?}"))?;
                o.d.s.diag.mutations.push(format!(
                    "t={}ns write {} bytes at {addr:#x}",
                    o.d.s.clock.now_ns(),
                    data.len()
                ));
                Ok(json!({ "written": data.len() }))
            }
            "memory.search" => {
                let needle = match (
                    p.get("hex").and_then(|v| v.as_str()),
                    p.get("text").and_then(|v| v.as_str()),
                ) {
                    (Some(h), _) => hex_decode(h)?,
                    (None, Some(t)) => t.as_bytes().to_vec(),
                    _ => bail!("give hex or text"),
                };
                let limit = p.get("limit").and_then(|v| v.as_u64()).unwrap_or(64) as usize;
                let o = self.session()?;
                let hits = o.d.s.mem.search(&needle, 0, u32::MAX, limit);
                Ok(json!({ "hits": hits }))
            }
            "regs.get" => {
                let o = self.session()?;
                o.d.s.sync_regs();
                let tid = match p.get("tid") {
                    Some(_) => u32_param(p, "tid")?,
                    None => o.d.s.sched.current.ok_or_else(|| anyhow!("no current thread"))?,
                };
                let t =
                    o.d.s
                        .sched
                        .threads
                        .get(&tid)
                        .ok_or_else(|| anyhow!("no thread {tid}"))?;
                Ok(json!({ "tid": tid, "regs": t.regs }))
            }
            "regs.set" => {
                let o = self.session()?;
                o.d.s.sync_regs();
                let tid = match p.get("tid") {
                    Some(_) => u32_param(p, "tid")?,
                    None => o.d.s.sched.current.ok_or_else(|| anyhow!("no current thread"))?,
                };
                let t =
                    o.d.s
                        .sched
                        .threads
                        .get_mut(&tid)
                        .ok_or_else(|| anyhow!("no thread {tid}"))?;
                let mut regs = serde_json::to_value(&t.regs)?;
                let changes = p["regs"].as_object().ok_or_else(|| anyhow!("regs must be an object"))?;
                for (k, v) in changes {
                    if regs.get(k).is_none() {
                        bail!("unknown register {k}");
                    }
                    regs[k] = v.clone();
                }
                t.regs = serde_json::from_value(regs)?;
                o.d.s.reload_regs().map_err(|e| anyhow!(e))?;
                o.d.s.diag.mutations.push(format!(
                    "t={}ns set registers {:?} of thread {tid}",
                    o.d.s.clock.now_ns(),
                    changes.keys().collect::<Vec<_>>()
                ));
                Ok(json!({}))
            }
            "breakpoint.add" => {
                let addr = u32_param(p, "addr")?;
                self.session()?.d.s.cpu.add_breakpoint(addr).map_err(|e| anyhow!(e))?;
                Ok(json!({}))
            }
            "breakpoint.remove" => {
                let addr = u32_param(p, "addr")?;
                let removed = self
                    .session()?
                    .d
                    .s
                    .cpu
                    .remove_breakpoint(addr)
                    .map_err(|e| anyhow!(e))?;
                Ok(json!({ "removed": removed }))
            }
            "watchpoint.add" => {
                let addr = u32_param(p, "addr")?;
                let len = p.get("len").map(|_| u32_param(p, "len")).transpose()?.unwrap_or(4);
                let write = p.get("write").and_then(|v| v.as_bool()).unwrap_or(true);
                let read = p.get("read").and_then(|v| v.as_bool()).unwrap_or(false);
                let o = self.session()?;
                o.d.s
                    .cpu
                    .add_watchpoint(addr, len, write, read)
                    .map_err(|e| anyhow!(e))?;
                Ok(json!({}))
            }
            "api_break.add" => {
                let name = p["name"].as_str().ok_or_else(|| anyhow!("missing name"))?.to_string();
                self.session()?.d.s.host.api_breaks.push(name);
                Ok(json!({}))
            }
            "api_break.remove" => {
                let name = p["name"].as_str().ok_or_else(|| anyhow!("missing name"))?;
                let o = self.session()?;
                o.d.s.host.api_breaks.retain(|n| !n.eq_ignore_ascii_case(name));
                Ok(json!({}))
            }
            "trace.set" => {
                let o = self.session()?;
                let s = &mut o.d.s;
                let api: Vec<String> = serde_json::from_value(p.get("api").cloned().unwrap_or(json!([])))?;
                if api.is_empty() {
                    s.diag.trace = None;
                } else {
                    s.diag.trace = Some(Default::default());
                    s.diag.trace_cap = p.get("cap").and_then(|v| v.as_u64()).unwrap_or(10_000) as usize;
                    s.diag.trace_filter.include = api.into_iter().filter(|t| t != "*").collect();
                }
                let blocks = p.get("blocks").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                s.cpu.set_trace(blocks);
                Ok(json!({}))
            }
            "trace.get" => {
                let o = self.session()?;
                let s = &o.d.s;
                Ok(json!({ "api": s.diag.trace, "blocks": s.cpu.state().trace }))
            }
            "savepoint.create" => {
                let name = p["name"].as_str().ok_or_else(|| anyhow!("missing name"))?.to_string();
                let o = self.session()?;
                let cp = o.d.s.checkpoint(&name);
                let info = cp.info.clone();
                o.ctx.savepoints.insert(name, cp);
                Ok(serde_json::to_value(info)?)
            }
            "savepoint.restore" => {
                let name = p["name"].as_str().ok_or_else(|| anyhow!("missing name"))?;
                let o = self.session()?;
                let cp = o
                    .ctx
                    .savepoints
                    .get(name)
                    .ok_or_else(|| anyhow!("no savepoint {name:?}"))?
                    .clone();
                o.d.s.restore(&cp, true).map_err(|e| anyhow!(e))?;
                o.d.reanchor();
                Ok(serde_json::to_value(&cp.info)?)
            }
            "savepoint.list" => {
                let o = self.session()?;
                let v: Vec<_> = o.ctx.savepoints.values().map(|c| c.info.clone()).collect();
                Ok(json!({ "savepoints": v }))
            }
            "savepoint.delete" => {
                let name = p["name"].as_str().ok_or_else(|| anyhow!("missing name"))?;
                Ok(json!({ "deleted": self.session()?.ctx.savepoints.remove(name).is_some() }))
            }
            "checkpoint.save" => {
                let path = PathBuf::from(p["path"].as_str().ok_or_else(|| anyhow!("missing path"))?);
                let label = p.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let o = self.session()?;
                let cp = o.d.s.checkpoint(&label);
                cp.write_to(&path).map_err(|e| anyhow!(e))?;
                Ok(json!({ "path": path, "bytes": std::fs::metadata(&path)?.len(), "info": cp.info }))
            }
            "checkpoint.load" => {
                let path = PathBuf::from(p["path"].as_str().ok_or_else(|| anyhow!("missing path"))?);
                let cp = Checkpoint::read_from(&path).map_err(|e| anyhow!(e))?;
                let o = self.session()?;
                let sha =
                    o.d.s
                        .modules
                        .get(o.d.s.process.exe_module)
                        .and_then(|m| m.sha256.clone());
                if cp.info.exe_sha256 != sha {
                    bail!("checkpoint was taken with a different executable");
                }
                o.d.s.restore(&cp, true).map_err(|e| anyhow!(e))?;
                o.d.reanchor();
                Ok(serde_json::to_value(&cp.info)?)
            }
            other => bail!("unknown method {other:?}"),
        }
    }

    fn open_session(&mut self, p: &Value) -> Result<Value> {
        let mut args = run_args_from_json(p)?;
        if let Some(o) = &self.open
            && !o.args.no_persist
        {
            o.d.s.persist().context("writing the previous session's overlay")?;
        }
        if p.get("pacing").is_none() {
            args.pacing = Pacing::Unthrottled;
        }
        let mut s = launch::build_session(&args)?;
        s.user.auto_dismiss_dialogs = args.auto_dismiss_dialogs;
        launch::configure_host(&mut s, &args)?;
        if !args.trace.is_empty() {
            s.diag.trace = Some(Default::default());
            s.diag.trace_cap = args.trace_cap;
            s.diag.trace_filter.include = args.trace.iter().filter(|t| *t != "*").cloned().collect();
        }
        if let Some(path) = &args.replay_input {
            launch::schedule_replay(&mut s, path)?;
        }
        let artifacts = args
            .artifacts
            .clone()
            .unwrap_or_else(|| PathBuf::from("local/artifacts/serve"));
        std::fs::create_dir_all(&artifacts)?;
        *self.cancel.lock().unwrap() = Some(s.host.cancel.clone());
        let info = json!({
            "exe": s.config.exe,
            "image_base": s.process.image_base,
            "virtual_s": s.clock.now_ns() as f64 / 1e9,
            "artifacts": artifacts,
        });
        let pacing = args.pacing;
        self.open = Some(Open {
            d: Driver::new(s, pacing),
            args,
            ctx: Ctx::default(),
            artifacts,
        });
        Ok(info)
    }

    fn run(&mut self, p: &Value) -> Result<Value> {
        let o = self.session()?;
        let seconds = p.get("seconds").and_then(|v| v.as_f64());
        let until_present = p.get("until_present").and_then(|v| v.as_bool()).unwrap_or(false);
        let frames = p.get("frames").and_then(|v| v.as_u64());
        let max_insns = p.get("max_insns").and_then(|v| v.as_u64());
        if seconds.is_none() && frames.is_none() && max_insns.is_none() && !until_present {
            bail!("give seconds, frames, max_insns or until_present");
        }
        let t0 = std::time::Instant::now();
        let start_ns = o.d.s.clock.now_ns();
        let deadline = seconds.map(|s| start_ns + (s * 1e9) as u64).unwrap_or(u64::MAX);
        let frame_goal = frames.map(|n| o.d.s.frames_presented + n);
        let stop = if let Some(n) = max_insns {
            o.d.s.run(RunLimit {
                until_ns: deadline,
                max_insns: Some(n),
                stop_on_present: until_present,
                jump_idle: true,
            })
        } else {
            loop {
                let now = o.d.s.clock.now_ns();
                if now >= deadline {
                    break RunStop::TimeReached;
                }
                // Advance in bounded chunks so frame goals are checked regularly.
                let chunk = (deadline - now).min(100_000_000);
                let r = o.d.advance(chunk, until_present || frame_goal.is_some());
                match r {
                    RunStop::TimeReached => continue,
                    RunStop::Presented { frame } => {
                        if until_present || frame_goal.is_some_and(|g| frame >= g) {
                            break r;
                        }
                    }
                    other => break other,
                }
            }
        };
        let s = &o.d.s;
        Ok(json!({
            "stop": stop,
            "virtual_s": s.clock.now_ns() as f64 / 1e9,
            "advanced_s": (s.clock.now_ns() - start_ns) as f64 / 1e9,
            "wall_s": t0.elapsed().as_secs_f64(),
            "frames_presented": s.frames_presented,
            "insns": s.cpu.state().insns,
        }))
    }
}

pub fn serve_stdio() -> Result<()> {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let cancel: Arc<Mutex<Option<Arc<AtomicBool>>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::channel::<Value>();
    {
        let out = out.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let req: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        send(
                            &out,
                            &json!({ "id": null, "error": { "code": "parse", "message": e.to_string() } }),
                        );
                        continue;
                    }
                };
                if req["method"] == "cancel" {
                    let flag = cancel.lock().unwrap().clone();
                    if let Some(f) = &flag {
                        f.store(true, Ordering::SeqCst);
                    }
                    send(
                        &out,
                        &json!({ "id": req["id"], "result": { "cancelled": flag.is_some() } }),
                    );
                    continue;
                }
                if tx.send(req).is_err() {
                    break;
                }
            }
        });
    }
    let mut srv = Server { open: None, cancel };
    for req in rx {
        let id = req["id"].clone();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        if method == "shutdown" {
            let r = match &srv.open {
                Some(o) if !o.args.no_persist => o.d.s.persist().map(|_| json!({})).map_err(|e| e.to_string()),
                _ => Ok(json!({})),
            };
            send(
                &out,
                &match r {
                    Ok(v) => json!({ "id": id, "result": v }),
                    Err(e) => {
                        json!({ "id": id, "error": { "code": "failed", "message": format!("writing the overlay: {e}") } })
                    }
                },
            );
            break;
        }
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| srv.handle(&method, &params)));
        let msg = match res {
            Ok(Ok(v)) => json!({ "id": id, "result": v }),
            Ok(Err(e)) => json!({ "id": id, "error": { "code": "failed", "message": format!("{e:#}") } }),
            Err(_) => {
                // A panic leaves the session in an unknown state; drop it.
                srv.open = None;
                json!({ "id": id, "error": { "code": "internal", "message": "internal error; the session was closed" } })
            }
        };
        send(&out, &msg);
    }
    Ok(())
}
