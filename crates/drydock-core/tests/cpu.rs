use drydock_core::cpu::{Cpu, FaultKind, GDT_BASE, Regs, Stop};
use drydock_core::mem::*;
use drydock_core::Reg;
use iced_x86::code_asm::*;

const CODE: u32 = 0x40_1000;
const STACK: u32 = 0x10_0000;
const TEB: u32 = 0x7ffd_e000;

struct M {
    cpu: Cpu,
    mem: AddressSpace,
}

fn asm(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(CODE as u64).unwrap()
}

fn machine(code: &[u8]) -> M {
    let mut mem = AddressSpace::new();
    mem.alloc(Some(0x40_0000), 0x10000, RegionKind::Image, PAGE_EXECUTE_READWRITE, "code").unwrap();
    mem.alloc(Some(STACK - 0x10000), 0x10000, RegionKind::Private, PAGE_READWRITE, "stack").unwrap();
    mem.alloc(Some(TEB), 0x1000, RegionKind::System, PAGE_READWRITE, "teb").unwrap();
    mem.alloc(Some(GDT_BASE), 0x1000, RegionKind::System, PAGE_READWRITE, "gdt").unwrap();
    mem.write_u32(TEB + 0x18, TEB).unwrap();
    mem.write_force(CODE, code).unwrap();
    let mut cpu = Cpu::new().unwrap();
    cpu.setup_gdt(&mut mem).unwrap();
    let regs = Regs::initial(CODE, STACK - 16, TEB);
    cpu.write_regs(&mut mem, &regs).unwrap();
    M { cpu, mem }
}

impl M {
    fn run(&mut self) -> Stop {
        self.cpu.run_slice(&mut self.mem, 1_000_000, 0).unwrap()
    }
}

#[test]
fn api_trap_reports_next_eip() {
    let code = asm(|a| {
        a.mov(eax, 5).unwrap();
        a.add(eax, 3).unwrap();
        a.int(0x2e).unwrap();
        a.nop().unwrap();
    });
    let mut m = machine(&code);
    let s = m.run();
    assert_eq!(s, Stop::ApiTrap { eip: CODE + code.len() as u32 - 1 });
    assert_eq!(m.cpu.reg(Reg::EAX), 8);
}

#[test]
fn budget_stops_infinite_loop_and_counts() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut l = a.create_label();
    a.set_label(&mut l).unwrap();
    a.inc(eax).unwrap();
    a.jmp(l).unwrap();
    let code = a.assemble(CODE as u64).unwrap();
    let mut m = machine(&code);
    let s = m.cpu.run_slice(&mut m.mem, 1000, 0).unwrap();
    assert_eq!(s, Stop::Budget);
    let n = m.cpu.state().insns;
    assert!((1000..1010).contains(&n), "insns={n}");
    // Two instructions per iteration, so EAX == iterations == insns / 2.
    assert_eq!(m.cpu.reg(Reg::EAX) as u64, n / 2);
}

#[test]
fn breakpoint_mid_block_stops_before_instruction() {
    let code = asm(|a| {
        a.nop().unwrap();
        a.nop().unwrap();
        a.mov(eax, 1).unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m = machine(&code);
    m.cpu.add_breakpoint(CODE + 2).unwrap();
    assert_eq!(m.run(), Stop::Breakpoint { addr: CODE + 2 });
    assert_eq!(m.cpu.eip(), CODE + 2);
    assert_eq!(m.cpu.reg(Reg::EAX), 0);
    // Resuming at the breakpoint address steps over it.
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), 1);
}

#[test]
fn unmapped_read_faults_at_instruction() {
    let code = asm(|a| {
        a.nop().unwrap();
        a.mov(eax, dword_ptr(0x10)).unwrap();
    });
    let mut m = machine(&code);
    match m.run() {
        Stop::Fault(f) => {
            assert_eq!(f.kind, FaultKind::ReadUnmapped);
            assert_eq!(f.addr, 0x10);
            assert_eq!(f.eip, CODE + 1);
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn write_to_readonly_page_faults() {
    let code = asm(|a| {
        a.mov(dword_ptr(0x40_8000), eax).unwrap();
    });
    let mut m = machine(&code);
    m.mem.protect(0x40_8000, 0x1000, PAGE_READONLY).unwrap();
    match m.run() {
        Stop::Fault(f) => assert_eq!(f.kind, FaultKind::WriteProt),
        s => panic!("{s:?}"),
    }
}

#[test]
fn divide_error_is_exception_0() {
    let code = asm(|a| {
        a.xor(ecx, ecx).unwrap();
        a.div(ecx).unwrap();
    });
    let mut m = machine(&code);
    match m.run() {
        Stop::Fault(f) => assert_eq!(f.kind, FaultKind::Exception(0)),
        s => panic!("{s:?}"),
    }
}

#[test]
fn fs_segment_points_at_teb() {
    let code = asm(|a| {
        a.mov(eax, dword_ptr(0x18).fs()).unwrap();
        a.mov(cx, fs).unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m = machine(&code);
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), TEB);
    assert_eq!(m.cpu.reg(Reg::ECX) & 0xffff, 0x3b);
}

#[test]
fn rdtsc_follows_virtual_time() {
    let code = asm(|a| {
        a.rdtsc().unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m = machine(&code);
    m.cpu.state_mut().tsc_hz = 1_000_000_000;
    let s = m.cpu.run_slice(&mut m.mem, 1000, 5_000_000_000).unwrap();
    assert!(matches!(s, Stop::ApiTrap { .. }));
    let tsc = (m.cpu.reg(Reg::EDX) as u64) << 32 | m.cpu.reg(Reg::EAX) as u64;
    // 5 s of virtual time at 1 GHz, plus at most a couple of instructions.
    assert!((5_000_000_000..5_000_000_010).contains(&tsc), "tsc={tsc}");
}

#[test]
fn x87_state_roundtrips_through_regs() {
    // Compute 1.5 * 3 = 4.5 and leave it on the stack, then also push pi.
    let code = asm(|a| {
        a.fld(dword_ptr(0x40_8000)).unwrap();
        a.fmul(dword_ptr(0x40_8004)).unwrap();
        a.fldpi().unwrap();
        a.fnstcw(word_ptr(0x40_8010)).unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m = machine(&code);
    m.mem.write_f32(0x40_8000, 1.5).unwrap();
    m.mem.write_f32(0x40_8004, 3.0).unwrap();
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.mem.read_u16(0x40_8010).unwrap(), 0x27f);
    let regs = m.cpu.read_regs();
    assert_ne!(regs.st, [[0u8; 10]; 8]);
    // TOP = 6 after two pushes.
    assert_eq!((regs.fpsw >> 11) & 7, 6);
    // Fresh CPU, restore, then store ST(1) (=4.5) and ST(0) (=pi).
    let code2 = asm(|a| {
        a.fstp(dword_ptr(0x40_8020)).unwrap();
        a.fstp(dword_ptr(0x40_8024)).unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m2 = machine(&code2);
    let mut r2 = regs.clone();
    r2.eip = CODE;
    m2.cpu.write_regs(&mut m2.mem, &r2).unwrap();
    assert_eq!(m2.cpu.read_regs().st, regs.st);
    assert!(matches!(m2.run(), Stop::ApiTrap { .. }));
    assert_eq!(m2.mem.read_f32(0x40_8020).unwrap(), std::f32::consts::PI);
    assert_eq!(m2.mem.read_f32(0x40_8024).unwrap(), 4.5);
}

#[test]
fn rust_side_code_write_invalidates_translation() {
    let code = asm(|a| {
        a.mov(eax, 1).unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m = machine(&code);
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), 1);
    // Patch the immediate (debugger-style write) and re-run from the start.
    m.mem.write_force(CODE + 1, &2u32.to_le_bytes()).unwrap();
    m.cpu.set_reg(Reg::EIP, CODE);
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), 2);
}

#[test]
fn guest_self_modifying_code_executes_new_bytes() {
    // Overwrite the immediate of a later `mov eax, imm32` and then execute it.
    let target = CODE + 0x40;
    let mut code = asm(|a| {
        a.mov(dword_ptr(target + 1), 7).unwrap();
        a.mov(ebx, target).unwrap();
        a.jmp(ebx).unwrap();
    });
    code.resize(0x40, 0x90);
    let tail = {
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(eax, 3).unwrap();
        a.int(0x2e).unwrap();
        a.assemble(target as u64).unwrap()
    };
    code.extend(tail);
    let mut m = machine(&code);
    // Execute once with the original bytes being translated first.
    m.cpu.set_reg(Reg::EIP, target);
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), 3);
    m.cpu.set_reg(Reg::EIP, CODE);
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), 7);
}


#[test]
fn stack_is_32_bit_after_segment_loads() {
    let code = asm(|a| {
        a.push(0x1234_5678).unwrap();
        a.pop(eax).unwrap();
        a.int(0x2e).unwrap();
    });
    let mut m = machine(&code);
    assert!(matches!(m.run(), Stop::ApiTrap { .. }));
    assert_eq!(m.cpu.reg(Reg::EAX), 0x1234_5678);
    assert_eq!(m.cpu.esp(), STACK - 16);
}
