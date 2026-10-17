use anyhow::{Context, Result, bail};
use drydock_win32::reg::Registry;
use drydock_win32::session::{RunLimit, RunStop, Session, SessionConfig, WindowsProfile};
use drydock_win32::vfs::{self, GuestPath, Vfs};
use std::path::PathBuf;

#[derive(clap::Args, Clone)]
pub struct RunArgs {
    /// Executable: a path relative to --root, or a host path inside it.
    #[arg(long)]
    pub exe: PathBuf,
    /// Host directory mounted read-only as the application directory.
    #[arg(long)]
    pub root: PathBuf,
    /// Guest path where --root is mounted.
    #[arg(long, default_value = "C:\\GAME")]
    pub mount: String,
    /// Extra command-line arguments passed to the program.
    #[arg(long, default_value = "")]
    pub args: String,
    /// Registry files (.reg) to import before start.
    #[arg(long = "reg")]
    pub reg: Vec<PathBuf>,
    /// Directory holding the persistent writable overlay (saves, settings).
    #[arg(long)]
    pub overlay: Option<PathBuf>,
    /// Stop after this much virtual time (seconds).
    #[arg(long, default_value_t = 30.0)]
    pub seconds: f64,
    /// Print a JSON report at the end.
    #[arg(long)]
    pub json: bool,
    /// Record API calls whose "MODULE!Name" contains one of these substrings.
    #[arg(long = "trace")]
    pub trace: Vec<String>,
    /// Size of the API trace ring.
    #[arg(long, default_value_t = 2000)]
    pub trace_cap: usize,
    /// Answer message boxes with their default button.
    #[arg(long)]
    pub auto_dismiss_dialogs: bool,
    /// Scenario file (JSON) to execute.
    #[arg(long)]
    pub scenario: Option<PathBuf>,
    /// Directory for reports, screenshots and captures.
    #[arg(long)]
    pub artifacts: Option<PathBuf>,
    /// Audio output: none (virtual device only), discard (mix and drop), device, or
    /// wav:<path> (capture). Several may be combined with '+'.
    #[arg(long, default_value = "none")]
    pub audio: String,
    /// Request the graphics backend's software (fallback) adapter.
    #[arg(long)]
    pub software_gpu: bool,
    /// Restrict graphics backends (vulkan, metal, gl, dx12).
    #[arg(long = "gpu-backend")]
    pub gpu_backend: Vec<String>,
    /// Attach a virtual joystick.
    #[arg(long)]
    pub joystick: bool,
    /// Host pacing of the virtual clock.
    #[arg(long, value_enum, default_value_t = crate::driver::Pacing::Unthrottled)]
    pub pacing: crate::driver::Pacing,
}

pub fn build_session(a: &RunArgs) -> Result<Session> {
    let root = a.root.canonicalize().with_context(|| format!("root {}", a.root.display()))?;
    let base = GuestPath { drive: 'C', comps: vec![] };
    let mount = vfs::normalize(&a.mount, &base).context("bad --mount")?;
    let exe_host = if a.exe.is_absolute() { a.exe.clone() } else { root.join(&a.exe) };
    let exe_host = exe_host.canonicalize().with_context(|| format!("exe {}", exe_host.display()))?;
    let rel = exe_host.strip_prefix(&root).context("--exe must be inside --root")?;
    let mut guest_exe = mount.clone();
    for c in rel.components() {
        guest_exe = guest_exe.join(&c.as_os_str().to_string_lossy());
    }
    let mut vfs = Vfs::new();
    vfs.mount(mount, root);
    if let Some(o) = &a.overlay {
        std::fs::create_dir_all(o)?;
        vfs.load_persisted(o)?;
    }
    let mut registry = Registry::new();
    for r in &a.reg {
        registry.import_reg(&std::fs::read(r)?).map_err(anyhow::Error::msg)?;
    }
    if let Some(o) = &a.overlay {
        let p = o.join("registry.reg");
        if p.exists() {
            registry.import_reg(&std::fs::read(p)?).map_err(anyhow::Error::msg)?;
        }
    }
    let config = SessionConfig {
        exe: guest_exe.display(),
        args: a.args.clone(),
        cwd: None,
        windows: WindowsProfile::default(),
        cpu: Default::default(),
        env: Vec::new(),
    };
    Session::new(config, vfs, registry).map_err(anyhow::Error::msg)
}

pub fn audio_sink(spec: &str) -> Result<Option<Box<dyn drydock_audio::AudioSink>>> {
    let mut sinks: Vec<Box<dyn drydock_audio::AudioSink>> = Vec::new();
    for part in spec.split('+') {
        match part {
            "none" | "" => {}
            "discard" => sinks.push(Box::new(drydock_audio::NullSink { frames: 0 })),
            "device" => sinks.push(Box::new(drydock_audio::DeviceSink::open().map_err(anyhow::Error::msg)?)),
            p if p.starts_with("wav:") => {
                sinks.push(Box::new(drydock_audio::WavSink::create(std::path::Path::new(&p[4..]))?))
            }
            other => bail!("unknown audio output {other:?}"),
        }
    }
    Ok(match sinks.len() {
        0 => None,
        1 => sinks.pop(),
        _ => Some(Box::new(drydock_audio::Tee(sinks))),
    })
}

pub fn configure_host(s: &mut Session, a: &RunArgs) -> Result<()> {
    s.host.gpu_options = drydock_gfx::GpuOptions {
        software: a.software_gpu,
        backends: a.gpu_backend.clone(),
    };
    s.host.audio = audio_sink(&a.audio)?;
    s.user.input.joy_present = a.joystick;
    Ok(())
}

pub fn provenance(s: &Session, a: &RunArgs) -> serde_json::Value {
    let modules: Vec<_> = s
        .modules
        .list
        .values()
        .map(|m| {
            serde_json::json!({
                "name": m.name, "path": m.path, "base": m.base, "builtin": m.builtin, "sha256": m.sha256,
            })
        })
        .collect();
    serde_json::json!({
        "drydock_version": env!("CARGO_PKG_VERSION"),
        "unicorn": "unicorn-engine 2.1.5",
        "host": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "exe": s.config.exe,
        "args": s.config.args,
        "windows_profile": s.config.windows,
        "cpu_profile": s.config.cpu,
        "pacing": a.pacing,
        "audio": a.audio,
        "gpu": s.host.renderer.as_ref().map(|r| serde_json::to_value(&r.gpu.report).unwrap()),
        "gpu_error": s.host.gpu_error,
        "modules": modules,
        "input_timeline": s.user.input.recorded,
        "debugger_mutations": s.diag.mutations,
        "scenario": a.scenario,
    })
}

fn top_apis(s: &Session) -> Vec<serde_json::Value> {
    let mut v: Vec<_> = s.diag.api_counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    v.into_iter().take(25).map(|(k, n)| serde_json::json!([k, n])).collect()
}

pub fn report(d: &crate::driver::Driver, stop: &RunStop) -> serde_json::Value {
    let s = &d.s;
    let wall = d.wall_start.elapsed().as_secs_f64();
    let virt = s.clock.now_ns() as f64 / 1e9;
    serde_json::json!({
        "stop": stop,
        "virtual_s": virt,
        "wall_s": wall,
        "speed": if wall > 0.0 { virt / wall } else { 0.0 },
        "insns": s.cpu.state().insns,
        "mips": s.cpu.state().insns as f64 / wall.max(1e-9) / 1e6,
        "frames_presented": s.frames_presented,
        "display_mode": s.user.mode,
        "halt": s.diag.halt,
        "missing": s.diag.missing,
        "events": s.diag.events.iter().rev().take(60).collect::<Vec<_>>(),
        "debug_output": s.diag.debug_output.iter().rev().take(40).collect::<Vec<_>>(),
        "windows": s.windows_info(),
        "api_top": top_apis(s),
        "api_calls": s.diag.api_counts.values().sum::<u64>(),
        "run_stats": s.stats,
        "threads": d.s.threads_info_static(),
        "trace": s.diag.trace,
    })
}

pub fn run(a: RunArgs) -> Result<()> {
    let mut s = build_session(&a)?;
    if !a.trace.is_empty() {
        s.diag.trace = Some(Default::default());
        s.diag.trace_cap = a.trace_cap;
        s.diag.trace_filter.include = a.trace.iter().filter(|t| *t != "*").cloned().collect();
    }
    s.user.auto_dismiss_dialogs = a.auto_dismiss_dialogs;
    configure_host(&mut s, &a)?;
    let artifacts = a.artifacts.clone().unwrap_or_else(|| PathBuf::from("local/artifacts/last"));
    std::fs::create_dir_all(&artifacts)?;
    let mut d = crate::driver::Driver::new(s, a.pacing);
    let (stop, scenario_log, ok) = if let Some(sp) = &a.scenario {
        let sc: crate::scenario::Scenario = serde_json::from_slice(&std::fs::read(sp)?)?;
        let out = crate::scenario::run(&mut d, &sc, &artifacts)?;
        (out.stop.unwrap_or(RunStop::TimeReached), Some(out.log), out.ok)
    } else {
        let stop = d.advance((a.seconds * 1e9) as u64, false);
        (stop, None, true)
    };
    let mut rep = report(&d, &stop);
    rep["scenario_ok"] = serde_json::json!(ok);
    rep["scenario_log"] = serde_json::json!(scenario_log);
    let prov = provenance(&d.s, &a);
    std::fs::write(artifacts.join("report.json"), serde_json::to_vec_pretty(&rep)?)?;
    std::fs::write(artifacts.join("provenance.json"), serde_json::to_vec_pretty(&prov)?)?;
    if let Some((w, h, px)) = d.s.capture_front() {
        std::fs::write(artifacts.join("final.png"), drydock_win32::observe::encode_png(w, h, &px))?;
    }
    println!("{}", serde_json::to_string_pretty(&rep)?);
    if d.s.diag.halt.is_some() {
        bail!("session halted");
    }
    if !ok {
        bail!("scenario failed");
    }
    Ok(())
}
