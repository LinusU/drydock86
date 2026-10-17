//! Savepoints: complete guest state captured between run slices.
//!
//! A [`Checkpoint`] holds everything the guest can observe — memory, registers of
//! every thread, kernel objects, windows, devices, the virtual clock — plus the
//! diagnostics recorded so far. Host-side bindings (GPU, audio output, cancellation)
//! are not part of it: after a restore the renderer re-uploads surfaces from guest
//! memory, which the capture makes authoritative by reading back GPU-only contents.
//!
//! Checkpoints live in memory (cheap clones) or on disk as MessagePack compressed
//! with deflate, behind a small header naming the format version.

use crate::api::{Conv, Handler};
use crate::session::{ProcessState, RunStats, Session, SessionConfig};
use crate::thunks::{ThunkInfo, ThunkRecord, Thunks};
use drydock_core::clock::VirtualClock;
use drydock_core::mem::RegionSnapshot;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"DRYDOCK\x01";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuCounters {
    pub insns: u64,
    pub tsc_time_base_ns: u64,
    pub tsc_insn_base: u64,
}

/// Identification shown before a checkpoint is loaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub format: u32,
    pub drydock_version: String,
    pub label: String,
    pub virtual_ns: u64,
    pub insns: u64,
    pub frames_presented: u64,
    pub exe: String,
    pub exe_sha256: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub info: CheckpointInfo,
    pub config: SessionConfig,
    cpu: CpuCounters,
    mem: Vec<RegionSnapshot>,
    clock: VirtualClock,
    sched: crate::sched::Sched,
    objects: crate::objects::Objects,
    modules: crate::loader::Modules,
    thunks: Vec<ThunkRecord>,
    thunk_specials: [u32; 3],
    vfs: crate::vfs::Vfs,
    registry: crate::reg::Registry,
    heaps: crate::heap::Heaps,
    process: ProcessState,
    user: crate::dll::user32::UserState,
    gdi: crate::dll::gdi32::GdiState,
    display: crate::surface::Display,
    winmm: crate::dll::winmm::MmState,
    #[serde(default)]
    vfw: crate::dll::msvfw32::VfwState,
    com: crate::com::ComState,
    diag: crate::diag::Diagnostics,
    cb_bufs: Vec<u32>,
    frames_presented: u64,
    pending_present_stop: bool,
    exited: Option<u32>,
    stats: RunStats,
}

/// Look up the Rust implementation behind a thunk by module and name.
fn handler_for(module: &str, name: &str) -> Option<(Conv, Option<Handler>)> {
    if let Some((iface, method)) = name.split_once("::") {
        let def = crate::com::ALL_IFACES
            .iter()
            .find(|d| d.name == iface && d.module.eq_ignore_ascii_case(module))?;
        let m = def.methods.iter().find(|m| m.name == method)?;
        return Some((m.conv, m.handler));
    }
    let dll = crate::dll::BUILTINS
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(module))?;
    let e = dll.parts.iter().flat_map(|p| p.iter()).find(|e| e.name == name)?;
    Some((e.conv, e.handler))
}

fn rebind(records: &[ThunkRecord], specials: [u32; 3]) -> Result<Thunks, String> {
    let mut t = Thunks::default();
    for r in records {
        let handler = if r.bound {
            match handler_for(&r.module, &r.name) {
                Some((_, Some(h))) => Some(h),
                _ => {
                    return Err(format!(
                        "checkpoint refers to {}!{}, which this build does not implement",
                        r.module, r.name
                    ));
                }
            }
        } else {
            None
        };
        t.insert(
            r.addr,
            ThunkInfo::new(r.module.clone(), r.name.clone(), r.ordinal, r.conv, handler),
        );
    }
    [t.callback_ret, t.thread_exit, t.thread_start] = specials;
    Ok(t)
}

impl Session {
    /// Capture the complete guest state. Must be called between [`Session::run`] calls.
    pub fn checkpoint(&mut self, label: &str) -> Checkpoint {
        self.sync_regs();
        // GPU-only surface contents become guest memory so the snapshot is complete.
        let ids: Vec<u32> = self
            .display
            .surfaces
            .iter()
            .filter(|(_, s)| s.gpu_dirty)
            .map(|(k, _)| *k)
            .collect();
        for id in ids {
            self.surface_cpu_access(id, false);
        }
        let st = self.cpu.state();
        let cpu = CpuCounters {
            insns: st.insns,
            tsc_time_base_ns: st.tsc_time_base_ns,
            tsc_insn_base: st.tsc_insn_base,
        };
        let exe_sha256 = self.modules.get(self.process.exe_module).and_then(|m| m.sha256.clone());
        Checkpoint {
            info: CheckpointInfo {
                format: FORMAT_VERSION,
                drydock_version: env!("CARGO_PKG_VERSION").to_string(),
                label: label.to_string(),
                virtual_ns: self.clock.now_ns(),
                insns: cpu.insns,
                frames_presented: self.frames_presented,
                exe: self.config.exe.clone(),
                exe_sha256,
            },
            config: self.config.clone(),
            cpu,
            mem: self.mem.snapshot(),
            clock: self.clock.clone(),
            sched: self.sched.clone(),
            objects: self.objects.clone(),
            modules: self.modules.clone(),
            thunks: self.thunks.records(),
            thunk_specials: [
                self.thunks.callback_ret,
                self.thunks.thread_exit,
                self.thunks.thread_start,
            ],
            vfs: self.vfs.clone(),
            registry: self.registry.clone(),
            heaps: self.heaps.clone(),
            process: self.process.clone(),
            user: self.user.clone(),
            gdi: self.gdi.clone(),
            display: self.display.clone(),
            winmm: self.winmm.clone(),
            vfw: self.vfw.clone(),
            com: self.com.clone(),
            diag: self.diag.clone(),
            cb_bufs: self.cb_bufs.clone(),
            frames_presented: self.frames_presented,
            pending_present_stop: self.pending_present_stop,
            exited: self.exited,
            stats: self.stats.clone(),
        }
    }

    /// Replace the guest state with a checkpoint. Host bindings (renderer, audio
    /// output, breakpoints) are kept; host mounts are kept when `keep_mounts` is set,
    /// so a checkpoint taken elsewhere runs against this machine's installation.
    pub fn restore(&mut self, cp: &Checkpoint, keep_mounts: bool) -> Result<(), String> {
        if cp.info.format != FORMAT_VERSION {
            return Err(format!(
                "checkpoint format {} is not supported (expected {FORMAT_VERSION})",
                cp.info.format
            ));
        }
        let thunks = rebind(&cp.thunks, cp.thunk_specials)?;
        self.mem.restore(&cp.mem);
        self.cpu.flush_mem_ops(&mut self.mem)?;
        self.cpu.flush_all_code()?;
        self.thunks = thunks;
        self.config = cp.config.clone();
        self.clock = cp.clock.clone();
        self.sched = cp.sched.clone();
        self.objects = cp.objects.clone();
        self.modules = cp.modules.clone();
        let mounts = std::mem::take(&mut self.vfs.mounts);
        let persist = self.vfs.persist_dir.take();
        self.vfs = cp.vfs.clone();
        self.vfs.persist_dir = persist;
        if keep_mounts {
            self.vfs.mounts = mounts;
        }
        self.registry = cp.registry.clone();
        self.heaps = cp.heaps.clone();
        self.process = cp.process.clone();
        self.user = cp.user.clone();
        self.gdi = cp.gdi.clone();
        self.display = cp.display.clone();
        self.winmm = cp.winmm.clone();
        self.vfw = cp.vfw.clone();
        self.com = cp.com.clone();
        self.diag = cp.diag.clone();
        self.cb_bufs = cp.cb_bufs.clone();
        self.frames_presented = cp.frames_presented;
        self.pending_present_stop = cp.pending_present_stop;
        self.exited = cp.exited;
        self.stats = cp.stats.clone();
        {
            let st = self.cpu.state_mut();
            st.insns = cp.cpu.insns;
            st.tsc_time_base_ns = cp.cpu.tsc_time_base_ns;
            st.tsc_insn_base = cp.cpu.tsc_insn_base;
            st.ns_per_insn_num = cp.config.cpu.ns_per_insn_num;
            st.ns_per_insn_den = cp.config.cpu.ns_per_insn_den;
            st.tsc_hz = cp.config.cpu.tsc_hz;
            st.stop = None;
            st.skip_breakpoint = None;
        }
        self.reload_regs()?;
        // Every surface is re-uploaded from guest memory on next use.
        for sf in self.display.surfaces.values_mut() {
            sf.cpu_dirty = true;
            sf.gpu_dirty = false;
        }
        self.host.gpu_versions.clear();
        if let Some(r) = self.host.renderer.as_mut() {
            r.drop_all_textures();
        }
        if let Some(a) = self.host.audio.as_mut() {
            a.flush();
        }
        Ok(())
    }
}

impl Checkpoint {
    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut w = std::io::BufWriter::new(f);
        w.write_all(MAGIC).map_err(|e| e.to_string())?;
        let info = serde_json::to_vec(&self.info).map_err(|e| e.to_string())?;
        w.write_all(&(info.len() as u32).to_le_bytes())
            .map_err(|e| e.to_string())?;
        w.write_all(&info).map_err(|e| e.to_string())?;
        let mut z = flate2::write::DeflateEncoder::new(w, flate2::Compression::fast());
        rmp_serde::encode::write(&mut z, self).map_err(|e| e.to_string())?;
        z.finish().and_then(|mut w| w.flush()).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Read only the header of a checkpoint file.
    pub fn read_info(path: &Path) -> Result<CheckpointInfo, String> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
        read_header(&mut r)
    }

    pub fn read_from(path: &Path) -> Result<Checkpoint, String> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
        let info = read_header(&mut r)?;
        if info.format != FORMAT_VERSION {
            return Err(format!(
                "checkpoint format {} is not supported (expected {FORMAT_VERSION})",
                info.format
            ));
        }
        let z = flate2::read::DeflateDecoder::new(r);
        rmp_serde::decode::from_read(z).map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn read_header(r: &mut impl Read) -> Result<CheckpointInfo, String> {
    let mut m = [0u8; 8];
    r.read_exact(&mut m).map_err(|e| e.to_string())?;
    if &m != MAGIC {
        return Err("not a Drydock checkpoint".into());
    }
    let mut n = [0u8; 4];
    r.read_exact(&mut n).map_err(|e| e.to_string())?;
    let mut info = vec![0u8; u32::from_le_bytes(n) as usize];
    r.read_exact(&mut info).map_err(|e| e.to_string())?;
    serde_json::from_slice(&info).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{RunLimit, RunStop};
    use drydock_fixtures::PeBuilder;
    use iced_x86::code_asm::*;

    /// Increments a counter, sleeping 1 ms between steps, and exits with the counter.
    fn counter_pe() -> Vec<u8> {
        PeBuilder::new()
            .import("KERNEL32.DLL", &["Sleep", "ExitProcess", "GetTickCount"])
            .bss(16)
            .build(|a, l| {
                let mut top = a.create_label();
                a.set_label(&mut top)?;
                a.inc(dword_ptr(l.data))?;
                a.call(dword_ptr(l.import("KERNEL32.DLL", "GetTickCount")))?;
                a.mov(dword_ptr(l.data + 4), eax)?;
                a.push(1)?;
                a.call(dword_ptr(l.import("KERNEL32.DLL", "Sleep")))?;
                a.cmp(dword_ptr(l.data), 500)?;
                a.jb(top)?;
                a.push(dword_ptr(l.data))?;
                a.call(dword_ptr(l.import("KERNEL32.DLL", "ExitProcess")))?;
                a.int3()
            })
    }

    fn run_to(s: &mut Session, ms: u64) -> RunStop {
        s.run(RunLimit {
            until_ns: ms * 1_000_000,
            max_insns: None,
            stop_on_present: false,
            jump_idle: true,
        })
    }

    fn observe(s: &mut Session) -> (u64, u64, u32, u32) {
        let data = drydock_fixtures::IMAGE_BASE + 0x2000;
        (
            s.clock.now_ns(),
            s.cpu.state().insns,
            s.mem.read_u32(data).unwrap(),
            s.mem.read_u32(data + 4).unwrap(),
        )
    }

    #[test]
    fn restore_reproduces_the_same_execution() {
        let pe = counter_pe();
        let mut s = crate::testutil::session_with(&pe);
        assert_eq!(run_to(&mut s, 50), RunStop::TimeReached);
        let cp = s.checkpoint("t50");
        let at_cp = observe(&mut s);
        assert!(
            at_cp.2 > 2,
            "the program made progress before the checkpoint: {at_cp:?}"
        );
        assert_eq!(run_to(&mut s, 200), RunStop::TimeReached);
        let a = observe(&mut s);
        assert!(a.2 > at_cp.2);

        // In-memory restore into the same session.
        s.restore(&cp, true).unwrap();
        assert_eq!(observe(&mut s), at_cp);
        assert_eq!(run_to(&mut s, 200), RunStop::TimeReached);
        assert_eq!(observe(&mut s), a);

        // Through a file, into a fresh session.
        let path = std::env::temp_dir().join(format!("drydock-ckpt-{}.ddck", std::process::id()));
        cp.write_to(&path).unwrap();
        let info = Checkpoint::read_info(&path).unwrap();
        assert_eq!(info.label, "t50");
        assert_eq!(info.virtual_ns, at_cp.0);
        let cp2 = Checkpoint::read_from(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let mut t = crate::testutil::session_with(&pe);
        t.restore(&cp2, true).unwrap();
        assert_eq!(observe(&mut t), at_cp);
        assert_eq!(run_to(&mut t, 200), RunStop::TimeReached);
        assert_eq!(observe(&mut t), a);
        let end = run_to(&mut t, 10_000);
        assert_eq!(end, RunStop::Exited { code: 500 });
    }
}
