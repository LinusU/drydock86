//! Unicorn-backed x86 CPU.
//!
//! Execution happens in bounded slices (`run_slice`). A slice ends when:
//! * the guest executes the API trap instruction (`int 0x2e`) in a runtime thunk,
//! * the instruction budget is exhausted (checked at translation-block entry),
//! * a breakpoint/watchpoint fires, or
//! * the guest faults (unmapped/protected access, invalid opcode, CPU exception).
//!
//! Instruction accounting uses a block hook: each executed translation block adds the
//! number of instructions it contains (decoded once with iced-x86 and cached). This is
//! the unit of guest CPU time. A block that faults part way is still charged in full;
//! breakpoints that stop inside a block cause the remainder to be charged again on
//! resume. Both effects are deterministic for a given build and input.

use crate::mem::{AddressSpace, MapOp, protect_to_uc};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use unicorn_engine::{
    Arch, HookType, MemType, Mode, Prot, RegisterX86 as R, Unicorn, X86CpuModel, uc_error,
};

/// Interrupt vector used by runtime thunks to enter Rust.
pub const API_TRAP_VECTOR: u32 = 0x2e;

/// Selector used for the FS (TEB) segment, as on Windows NT.
pub const FS_SELECTOR: u16 = 0x3b;
pub const GDT_BASE: u32 = 0x8000_0000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultKind {
    ReadUnmapped,
    WriteUnmapped,
    FetchUnmapped,
    ReadProt,
    WriteProt,
    FetchProt,
    InvalidInsn,
    /// CPU exception vector (e.g. 0 = #DE, 1 = #DB, 3 = #BP, 13 = #GP).
    Exception(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fault {
    pub kind: FaultKind,
    pub eip: u32,
    pub addr: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stop {
    /// `int 0x2e` executed; `eip` is the address *after* the trap instruction.
    ApiTrap { eip: u32 },
    Budget,
    Breakpoint { addr: u32 },
    Watchpoint { addr: u32, size: u32, write: bool, eip: u32 },
    Fault(Fault),
    /// Stop requested from outside guest execution (e.g. cancellation).
    Requested,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub eip: u32,
    pub insns: u64,
}

/// State touched from inside Unicorn hooks.
pub struct HookState {
    pub stop: Option<Stop>,
    /// Instructions retired since the CPU was created (or restored).
    pub insns: u64,
    /// Stop at the first block boundary where `insns >= insn_limit`.
    pub insn_limit: u64,
    icache: FxHashMap<u32, (u32, u32)>,
    /// Bitset of 4 KiB pages that have held translated code since the last full flush.
    /// Writes elsewhere need no translation-cache invalidation.
    code_pages: Vec<u64>,
    pub breakpoints: FxHashMap<u32, unicorn_engine::UcHookId>,
    pub skip_breakpoint: Option<u32>,
    /// Virtual time (ns) corresponding to `tsc_insn_base`.
    pub tsc_time_base_ns: u64,
    pub tsc_insn_base: u64,
    /// Guest nanoseconds per retired instruction, as a rational (num/den).
    pub ns_per_insn_num: u64,
    pub ns_per_insn_den: u64,
    pub tsc_hz: u64,
    /// Optional bounded execution trace (block granularity).
    pub trace: Option<std::collections::VecDeque<TraceEntry>>,
    pub trace_cap: usize,
    pub blocks: u64,
}

impl HookState {
    /// Whether any page of `[addr, addr + len)` has held translated code.
    fn has_code(&self, addr: u32, len: u32) -> bool {
        let last = (addr as u64 + len as u64 - 1).min(u32::MAX as u64) as u32;
        ((addr >> 12)..=(last >> 12)).any(|p| self.code_pages[p as usize >> 6] & (1 << (p & 63)) != 0)
    }

    /// Virtual time "now" as seen from inside a slice (for RDTSC).
    pub fn now_ns(&self) -> u64 {
        let d = self.insns - self.tsc_insn_base;
        self.tsc_time_base_ns + d * self.ns_per_insn_num / self.ns_per_insn_den
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Regs {
    pub eax: u32,
    pub ecx: u32,
    pub edx: u32,
    pub ebx: u32,
    pub esp: u32,
    pub ebp: u32,
    pub esi: u32,
    pub edi: u32,
    pub eip: u32,
    pub eflags: u32,
    pub fs_base: u32,
    pub fpcw: u16,
    pub fpsw: u16,
    pub fptag: u16,
    pub fop: u16,
    pub fip: u32,
    pub fdp: u32,
    /// Physical x87 registers R0..R7 (80-bit, little-endian mantissa then exponent).
    pub st: [[u8; 10]; 8],
    pub mxcsr: u32,
    pub xmm: [[u8; 16]; 8],
}

impl Regs {
    /// Initial register state for a new Win32 thread.
    pub fn initial(eip: u32, esp: u32, teb: u32) -> Regs {
        Regs {
            eip,
            esp,
            eflags: 0x202,
            fs_base: teb,
            // Windows initialises the x87 control word to 0x27f (53-bit precision,
            // all exceptions masked) and MXCSR to 0x1f80.
            fpcw: 0x27f,
            fptag: 0xffff,
            mxcsr: 0x1f80,
            ..Default::default()
        }
    }
}

pub struct Cpu {
    pub uc: Unicorn<'static, HookState>,
    rdtsc_hook: unicorn_engine::uc_hook,
    /// Base of the FS descriptor currently loaded (the running thread's TEB).
    fs_base: u32,
}

fn uc_err(e: uc_error) -> String {
    format!("{e:?}")
}

unsafe extern "C" fn rdtsc_cb(uc: *mut unicorn_engine::uc_engine, user: *mut c_void) -> i32 {
    let st = unsafe { &*(user as *const HookState) };
    let tsc = (st.now_ns() as u128 * st.tsc_hz as u128 / 1_000_000_000u128) as u64;
    let lo = tsc as u32 as u64;
    let hi = (tsc >> 32) as u64;
    unsafe {
        unicorn_engine::uc_reg_write(uc, R::EAX as i32, (&raw const lo).cast());
        unicorn_engine::uc_reg_write(uc, R::EDX as i32, (&raw const hi).cast());
    }
    1
}

fn count_insns(bytes: &[u8], ip: u32) -> u32 {
    let mut dec = iced_x86::Decoder::with_ip(32, bytes, ip as u64, iced_x86::DecoderOptions::NONE);
    let mut n = 0;
    let mut ins = iced_x86::Instruction::default();
    while dec.can_decode() {
        dec.decode_out(&mut ins);
        n += 1;
    }
    n.max(1)
}

impl Cpu {
    pub fn new() -> Result<Cpu, String> {
        let hs = HookState {
            stop: None,
            insns: 0,
            insn_limit: u64::MAX,
            icache: FxHashMap::default(),
            breakpoints: FxHashMap::default(),
            skip_breakpoint: None,
            tsc_time_base_ns: 0,
            code_pages: vec![0; 1 << 14],
            tsc_insn_base: 0,
            ns_per_insn_num: 1,
            ns_per_insn_den: 1,
            tsc_hz: 1_000_000_000,
            trace: None,
            trace_cap: 0,
            blocks: 0,
        };
        let mut uc = Unicorn::new_with_data(Arch::X86, Mode::MODE_32, hs).map_err(uc_err)?;
        uc.ctl_set_cpu_model(X86CpuModel::PENTIUM3 as i32)
            .map_err(uc_err)?;

        uc.add_block_hook(1, 0, |uc, addr, size| {
            let st = uc.get_data_mut();
            if st.insns >= st.insn_limit {
                if st.stop.is_none() {
                    st.stop = Some(Stop::Budget);
                }
                let _ = uc.emu_stop();
                return;
            }
            let addr = addr as u32;
            let n = match st.icache.get(&addr) {
                Some(&(s, n)) if s == size => n,
                _ => {
                    let mut buf = vec![0u8; size as usize];
                    let n = if uc.mem_read(addr as u64, &mut buf).is_ok() {
                        count_insns(&buf, addr)
                    } else {
                        1
                    };
                    let st = uc.get_data_mut();
                    st.icache.insert(addr, (size, n));
                    let last = addr.saturating_add(size.max(1) - 1);
                    for page in (addr >> 12)..=(last >> 12) {
                        st.code_pages[page as usize >> 6] |= 1 << (page & 63);
                    }
                    n
                }
            };
            let st = uc.get_data_mut();
            st.insns += n as u64;
            st.blocks += 1;
            if let Some(t) = st.trace.as_mut() {
                if t.len() >= st.trace_cap {
                    t.pop_front();
                }
                t.push_back(TraceEntry {
                    eip: addr,
                    insns: st.insns,
                });
            }
        })
        .map_err(uc_err)?;

        uc.add_intr_hook(|uc, intno| {
            let eip = uc.reg_read(R::EIP).unwrap_or(0) as u32;
            let st = uc.get_data_mut();
            if st.stop.is_none() {
                st.stop = Some(if intno == API_TRAP_VECTOR {
                    Stop::ApiTrap { eip }
                } else {
                    Stop::Fault(Fault {
                        kind: FaultKind::Exception(intno),
                        eip,
                        addr: 0,
                        size: 0,
                    })
                });
            }
            let _ = uc.emu_stop();
        })
        .map_err(uc_err)?;

        uc.add_mem_hook(HookType::MEM_INVALID, 1, 0, |uc, ty, addr, size, _v| {
            let eip = uc.reg_read(R::EIP).unwrap_or(0) as u32;
            let kind = match ty {
                MemType::READ_UNMAPPED => FaultKind::ReadUnmapped,
                MemType::WRITE_UNMAPPED => FaultKind::WriteUnmapped,
                MemType::FETCH_UNMAPPED => FaultKind::FetchUnmapped,
                MemType::READ_PROT => FaultKind::ReadProt,
                MemType::WRITE_PROT => FaultKind::WriteProt,
                MemType::FETCH_PROT => FaultKind::FetchProt,
                _ => FaultKind::ReadUnmapped,
            };
            let st = uc.get_data_mut();
            if st.stop.is_none() {
                st.stop = Some(Stop::Fault(Fault {
                    kind,
                    eip,
                    addr: addr as u32,
                    size: size as u32,
                }));
            }
            false
        })
        .map_err(uc_err)?;

        uc.add_insn_invalid_hook(|uc| {
            let eip = uc.reg_read(R::EIP).unwrap_or(0) as u32;
            let st = uc.get_data_mut();
            if st.stop.is_none() {
                st.stop = Some(Stop::Fault(Fault {
                    kind: FaultKind::InvalidInsn,
                    eip,
                    addr: eip,
                    size: 0,
                }));
            }
            false
        })
        .map_err(uc_err)?;

        let mut cpu = Cpu { uc, rdtsc_hook: 0, fs_base: 0 };
        cpu.install_rdtsc_hook()?;
        Ok(cpu)
    }

    fn install_rdtsc_hook(&mut self) -> Result<(), String> {
        let user = self.uc.get_data_mut() as *mut HookState as *mut c_void;
        let mut hh: unicorn_engine::uc_hook = 0;
        let err = unsafe {
            unicorn_engine::uc_hook_add(
                self.uc.get_handle(),
                &mut hh,
                HookType::INSN.0 as i32,
                rdtsc_cb as *mut c_void,
                user,
                1u64,
                0u64,
                unicorn_engine::X86Insn::RDTSC as i32,
            )
        };
        if err != uc_error::OK {
            return Err(format!("rdtsc hook: {err:?}"));
        }
        self.rdtsc_hook = hh;
        Ok(())
    }

    pub fn state(&self) -> &HookState {
        self.uc.get_data()
    }
    pub fn state_mut(&mut self) -> &mut HookState {
        self.uc.get_data_mut()
    }

    /// Install a flat GDT and load the data segment registers from it. The GDT page must
    /// already be committed at [`GDT_BASE`] in the address space.
    ///
    /// Any segment load makes QEMU recompute the stack-size flag from SS's descriptor, so
    /// SS must be a real flat 32-bit segment before FS is loaded. Unicorn runs guest code
    /// at CPL 0, so SS uses a ring-0 selector; DS/ES use the NT user selector 0x23.
    pub fn setup_gdt(&mut self, mem: &mut AddressSpace) -> Result<(), String> {
        fn desc(base: u32, limit: u32, access: u8, flags: u8) -> [u8; 8] {
            [
                limit as u8,
                (limit >> 8) as u8,
                base as u8,
                (base >> 8) as u8,
                (base >> 16) as u8,
                access,
                (flags << 4) | ((limit >> 16) as u8 & 0xf),
                (base >> 24) as u8,
            ]
        }
        let mut gdt = [0u8; 64];
        gdt[8..16].copy_from_slice(&desc(0, 0xfffff, 0x9a, 0xc)); // 0x08 ring-0 code
        gdt[16..24].copy_from_slice(&desc(0, 0xfffff, 0x92, 0xc)); // 0x10 ring-0 data
        gdt[24..32].copy_from_slice(&desc(0, 0xfffff, 0xfa, 0xc)); // 0x1b user code
        gdt[32..40].copy_from_slice(&desc(0, 0xfffff, 0xf2, 0xc)); // 0x23 user data
        mem.write_force(GDT_BASE, &gdt).map_err(|e| e.to_string())?;
        mem.pending_invalidate.pop();
        self.flush_mem_ops(mem)?;
        // uc_x86_mmr { u16 selector; u64 base; u32 limit; u32 flags }
        let mut mmr = [0u8; 24];
        mmr[8..16].copy_from_slice(&(GDT_BASE as u64).to_le_bytes());
        mmr[16..20].copy_from_slice(&63u32.to_le_bytes());
        self.uc.reg_write_long(R::GDTR, &mmr).map_err(uc_err)?;
        self.uc.reg_write(R::SS, 0x10).map_err(uc_err)?;
        self.uc.reg_write(R::DS, 0x23).map_err(uc_err)?;
        self.uc.reg_write(R::ES, 0x23).map_err(uc_err)?;
        Ok(())
    }

    fn write_fs_descriptor(&mut self, mem: &mut AddressSpace, base: u32) -> Result<(), String> {
        let limit: u32 = 0xfff;
        let mut d = [0u8; 8];
        d[0] = limit as u8;
        d[1] = (limit >> 8) as u8;
        d[2] = base as u8;
        d[3] = (base >> 8) as u8;
        d[4] = (base >> 16) as u8;
        d[5] = 0xf3; // present, DPL3, data RW accessed
        d[6] = 0x40 | ((limit >> 16) as u8 & 0xf); // 32-bit, byte granular
        d[7] = (base >> 24) as u8;
        mem.write_force(GDT_BASE + 7 * 8, &d).map_err(|e| e.to_string())?;
        // GDT is runtime-owned data, never code: no invalidation needed.
        mem.pending_invalidate.pop();
        Ok(())
    }

    /// Apply pending mapping changes and code invalidations to Unicorn.
    pub fn flush_mem_ops(&mut self, mem: &mut AddressSpace) -> Result<(), String> {
        for op in mem.pending_map.drain(..) {
            match op {
                MapOp::Map { base, size, ptr } => unsafe {
                    self.uc
                        .mem_map_ptr(base as u64, size as u64, Prot::NONE, ptr as *mut c_void)
                        .map_err(|e| format!("map {base:#x}+{size:#x}: {e:?}"))?;
                },
                MapOp::Unmap { base, size } => {
                    self.uc
                        .mem_unmap(base as u64, size as u64)
                        .map_err(|e| format!("unmap {base:#x}: {e:?}"))?;
                }
                MapOp::Protect {
                    base,
                    size,
                    protect,
                    committed,
                } => {
                    let p = if committed { protect_to_uc(protect) } else { Prot::NONE };
                    self.uc
                        .mem_protect(base as u64, size as u64, p)
                        .map_err(|e| format!("protect {base:#x}: {e:?}"))?;
                }
            }
        }
        let inv: Vec<_> = mem.pending_invalidate.drain(..).collect();
        for (a, l) in inv {
            if l == 0 {
                continue;
            }
            let end = (a as u64 + l as u64).min(u32::MAX as u64);
            self.uc.ctl_remove_cache(a as u64, end).map_err(uc_err)?;
            let st = self.uc.get_data_mut();
            if l != u32::MAX && !self.uc.get_data().has_code(a, l) {
                continue;
            }
            if l == u32::MAX {
                st.icache.clear();
            } else if !st.icache.is_empty() {
                // Drop cached instruction counts for blocks starting in the range.
                st.icache.retain(|&k, _| (k as u64) < a as u64 || k as u64 >= end);
            }
        }
        Ok(())
    }

    pub fn reg(&self, r: R) -> u32 {
        self.uc.reg_read(r).unwrap_or(0) as u32
    }
    pub fn set_reg(&mut self, r: R, v: u32) {
        let _ = self.uc.reg_write(r, v as u64);
    }
    pub fn eip(&self) -> u32 {
        self.reg(R::EIP)
    }
    pub fn esp(&self) -> u32 {
        self.reg(R::ESP)
    }

    pub fn read_regs(&self) -> Regs {
        let r = |x| self.uc.reg_read(x).unwrap_or(0) as u32;
        let mut st = [[0u8; 10]; 8];
        for (i, s) in st.iter_mut().enumerate() {
            let mut buf = [0u8; 16];
            unsafe {
                unicorn_engine::uc_reg_read(
                    self.uc.get_handle(),
                    R::FP0 as i32 + i as i32,
                    buf.as_mut_ptr().cast(),
                );
            }
            s.copy_from_slice(&buf[..10]);
        }
        let mut xmm = [[0u8; 16]; 8];
        for (i, x) in xmm.iter_mut().enumerate() {
            let mut buf = [0u8; 16];
            unsafe {
                unicorn_engine::uc_reg_read(
                    self.uc.get_handle(),
                    R::XMM0 as i32 + i as i32,
                    buf.as_mut_ptr().cast(),
                );
            }
            x.copy_from_slice(&buf);
        }
        Regs {
            eax: r(R::EAX),
            ecx: r(R::ECX),
            edx: r(R::EDX),
            ebx: r(R::EBX),
            esp: r(R::ESP),
            ebp: r(R::EBP),
            esi: r(R::ESI),
            edi: r(R::EDI),
            eip: r(R::EIP),
            eflags: r(R::EFLAGS),
            fs_base: self.fs_base,
            fpcw: r(R::FPCW) as u16,
            fpsw: r(R::FPSW) as u16,
            fptag: r(R::FPTAG) as u16,
            fop: r(R::FOP) as u16,
            fip: r(R::FIP),
            fdp: r(R::FDP),
            st,
            mxcsr: r(R::MXCSR),
            xmm,
        }
    }

    pub fn write_regs(&mut self, mem: &mut AddressSpace, g: &Regs) -> Result<(), String> {
        self.write_fs_descriptor(mem, g.fs_base)?;
        self.flush_mem_ops(mem)?;
        self.fs_base = g.fs_base;
        let w = |uc: &mut Unicorn<HookState>, x: R, v: u32| uc.reg_write(x, v as u64);
        let uc = &mut self.uc;
        w(uc, R::EAX, g.eax).map_err(uc_err)?;
        w(uc, R::ECX, g.ecx).map_err(uc_err)?;
        w(uc, R::EDX, g.edx).map_err(uc_err)?;
        w(uc, R::EBX, g.ebx).map_err(uc_err)?;
        w(uc, R::ESP, g.esp).map_err(uc_err)?;
        w(uc, R::EBP, g.ebp).map_err(uc_err)?;
        w(uc, R::ESI, g.esi).map_err(uc_err)?;
        w(uc, R::EDI, g.edi).map_err(uc_err)?;
        w(uc, R::EIP, g.eip).map_err(uc_err)?;
        w(uc, R::EFLAGS, g.eflags).map_err(uc_err)?;
        w(uc, R::FS, FS_SELECTOR as u32).map_err(uc_err)?;
        for (i, s) in g.st.iter().enumerate() {
            uc.reg_write_long(R::FP0 as i32 + i as i32, s).map_err(uc_err)?;
        }
        w(uc, R::FPCW, g.fpcw as u32).map_err(uc_err)?;
        w(uc, R::FPSW, g.fpsw as u32).map_err(uc_err)?;
        w(uc, R::FPTAG, g.fptag as u32).map_err(uc_err)?;
        w(uc, R::FOP, g.fop as u32).map_err(uc_err)?;
        w(uc, R::FIP, g.fip).map_err(uc_err)?;
        w(uc, R::FDP, g.fdp).map_err(uc_err)?;
        w(uc, R::MXCSR, g.mxcsr).map_err(uc_err)?;
        for (i, x) in g.xmm.iter().enumerate() {
            uc.reg_write_long(R::XMM0 as i32 + i as i32, x).map_err(uc_err)?;
        }
        Ok(())
    }

    /// Run guest code from the current EIP until a stop condition.
    ///
    /// `insn_budget` bounds the number of instructions (checked at block boundaries).
    /// `time_base_ns` is the virtual time at the start of the slice, used by RDTSC.
    pub fn run_slice(
        &mut self,
        mem: &mut AddressSpace,
        insn_budget: u64,
        time_base_ns: u64,
    ) -> Result<Stop, String> {
        self.flush_mem_ops(mem)?;
        let eip = self.eip();
        {
            let st = self.uc.get_data_mut();
            st.stop = None;
            st.insn_limit = st.insns.saturating_add(insn_budget);
            st.tsc_time_base_ns = time_base_ns;
            st.tsc_insn_base = st.insns;
            if st.breakpoints.contains_key(&eip) {
                st.skip_breakpoint = Some(eip);
            }
        }
        // `until` is an address in the never-mapped top page so it is never reached.
        let res = self.uc.emu_start(eip as u64, 0xffff_fff0, 0, 0);
        let st = self.uc.get_data_mut();
        st.skip_breakpoint = None;
        if let Some(s) = st.stop.take() {
            return Ok(s);
        }
        match res {
            Ok(()) => Ok(Stop::Requested),
            Err(e) => {
                let eip = self.eip();
                Ok(Stop::Fault(Fault {
                    kind: match e {
                        uc_error::INSN_INVALID => FaultKind::InvalidInsn,
                        uc_error::FETCH_UNMAPPED => FaultKind::FetchUnmapped,
                        uc_error::FETCH_PROT => FaultKind::FetchProt,
                        uc_error::READ_UNMAPPED => FaultKind::ReadUnmapped,
                        uc_error::WRITE_UNMAPPED => FaultKind::WriteUnmapped,
                        uc_error::READ_PROT => FaultKind::ReadProt,
                        uc_error::WRITE_PROT => FaultKind::WriteProt,
                        _ => FaultKind::Exception(u32::MAX),
                    },
                    eip,
                    addr: eip,
                    size: 0,
                }))
            }
        }
    }

    pub fn add_breakpoint(&mut self, addr: u32) -> Result<(), String> {
        if self.uc.get_data().breakpoints.contains_key(&addr) {
            return Ok(());
        }
        let id = self
            .uc
            .add_code_hook(addr as u64, addr as u64, move |uc, a, _| {
                let st = uc.get_data_mut();
                if st.skip_breakpoint == Some(a as u32) {
                    st.skip_breakpoint = None;
                    return;
                }
                if st.stop.is_none() {
                    st.stop = Some(Stop::Breakpoint { addr: a as u32 });
                }
                let _ = uc.emu_stop();
            })
            .map_err(uc_err)?;
        self.uc.get_data_mut().breakpoints.insert(addr, id);
        // Existing translations of this address must be regenerated with the hook.
        self.uc
            .ctl_remove_cache(addr as u64, addr as u64 + 1)
            .map_err(uc_err)?;
        Ok(())
    }

    pub fn remove_breakpoint(&mut self, addr: u32) -> Result<bool, String> {
        if let Some(id) = self.uc.get_data_mut().breakpoints.remove(&addr) {
            self.uc.remove_hook(id).map_err(uc_err)?;
            self.uc
                .ctl_remove_cache(addr as u64, addr as u64 + 1)
                .map_err(uc_err)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Install a watchpoint; returns the hook id (remove with `remove_hook`).
    pub fn add_watchpoint(
        &mut self,
        addr: u32,
        len: u32,
        write: bool,
        read: bool,
    ) -> Result<unicorn_engine::UcHookId, String> {
        let mut ty = HookType(0);
        if write {
            ty |= HookType::MEM_WRITE;
        }
        if read {
            ty |= HookType::MEM_READ;
        }
        self.uc
            .add_mem_hook(
                ty,
                addr as u64,
                addr as u64 + len.max(1) as u64 - 1,
                |uc, mt, a, size, _| {
                    let eip = uc.reg_read(R::EIP).unwrap_or(0) as u32;
                    let st = uc.get_data_mut();
                    if st.stop.is_none() {
                        st.stop = Some(Stop::Watchpoint {
                            addr: a as u32,
                            size: size as u32,
                            write: mt == MemType::WRITE,
                            eip,
                        });
                    }
                    let _ = uc.emu_stop();
                    true
                },
            )
            .map_err(uc_err)
    }

    pub fn remove_hook(&mut self, id: unicorn_engine::UcHookId) -> Result<(), String> {
        self.uc.remove_hook(id).map_err(uc_err)
    }

    /// Enable/disable the bounded block trace.
    pub fn set_trace(&mut self, cap: usize) {
        let st = self.uc.get_data_mut();
        st.trace_cap = cap;
        st.trace = if cap > 0 {
            Some(std::collections::VecDeque::with_capacity(cap))
        } else {
            None
        };
    }

    /// Flush every translated block (after restoring memory from a checkpoint).
    pub fn flush_all_code(&mut self) -> Result<(), String> {
        self.uc.ctl_flush_tb().map_err(uc_err)?;
        self.uc.get_data_mut().icache.clear();
        Ok(())
    }
}
