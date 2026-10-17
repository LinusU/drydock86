//! Rough throughput measurements; run with `cargo test --release -- --ignored --nocapture`.
use drydock_core::cpu::{Cpu, GDT_BASE, Regs, Stop};
use drydock_core::mem::*;
use iced_x86::code_asm::*;
use std::time::Instant;

fn setup(code: &[u8]) -> (Cpu, AddressSpace) {
    let mut mem = AddressSpace::new();
    mem.alloc(Some(0x40_0000), 0x10000, RegionKind::Image, PAGE_EXECUTE_READWRITE, "code").unwrap();
    mem.alloc(Some(0xf_0000), 0x10000, RegionKind::Private, PAGE_READWRITE, "stack").unwrap();
    mem.alloc(Some(GDT_BASE), 0x1000, RegionKind::System, PAGE_READWRITE, "gdt").unwrap();
    mem.write_force(0x40_1000, code).unwrap();
    let mut cpu = Cpu::new().unwrap();
    cpu.setup_gdt(&mut mem).unwrap();
    cpu.write_regs(&mut mem, &Regs::initial(0x40_1000, 0xf_fff0, 0)).unwrap();
    (cpu, mem)
}

#[test]
#[ignore]
fn bench_tight_loop() {
    // ~8 instructions per iteration with memory traffic.
    let mut a = CodeAssembler::new(32).unwrap();
    let mut l = a.create_label();
    a.mov(ecx, 50_000_000).unwrap();
    a.set_label(&mut l).unwrap();
    a.mov(eax, dword_ptr(0xf_0000)).unwrap();
    a.add(eax, ecx).unwrap();
    a.imul_2(eax, eax).unwrap();
    a.mov(dword_ptr(0xf_0000), eax).unwrap();
    a.xor(edx, eax).unwrap();
    a.shr(edx, 3).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(l).unwrap();
    a.int(0x2e).unwrap();
    let code = a.assemble(0x40_1000).unwrap();
    let (mut cpu, mut mem) = setup(&code);
    let t = Instant::now();
    let mut slices = 0;
    loop {
        slices += 1;
        match cpu.run_slice(&mut mem, 1_000_000, 0).unwrap() {
            Stop::Budget => continue,
            Stop::ApiTrap { .. } => break,
            s => panic!("{s:?}"),
        }
    }
    let dt = t.elapsed().as_secs_f64();
    let n = cpu.state().insns;
    eprintln!("tight loop: {n} insns in {dt:.3}s = {:.0} MIPS ({slices} slices, {} blocks)", n as f64 / dt / 1e6, cpu.state().blocks);
}

#[test]
#[ignore]
fn bench_api_trap_roundtrip() {
    // Each iteration traps once; the host side simulates a trivial stdcall return.
    let mut a = CodeAssembler::new(32).unwrap();
    let mut l = a.create_label();
    let mut thunk = a.create_label();
    a.mov(esi, 1_000_000).unwrap();
    a.set_label(&mut l).unwrap();
    a.push(1).unwrap();
    a.call(thunk).unwrap();
    a.dec(esi).unwrap();
    a.jnz(l).unwrap();
    a.int(0x2e).unwrap();
    a.set_label(&mut thunk).unwrap();
    a.int(0x2e).unwrap();
    let code = a.assemble(0x40_1000).unwrap();
    let (mut cpu, mut mem) = setup(&code);
    let t = Instant::now();
    let mut traps = 0u64;
    loop {
        match cpu.run_slice(&mut mem, u64::MAX / 2, 0).unwrap() {
            Stop::ApiTrap { eip } => {
                if cpu.reg(drydock_core::Reg::ESI) == 0 { break; }
                // stdcall ret 4
                let stack_ptr = cpu.esp();
                let ret = mem.read_u32(stack_ptr).unwrap();
                cpu.set_reg(drydock_core::Reg::ESP, stack_ptr + 8);
                cpu.set_reg(drydock_core::Reg::EIP, ret);
                traps += 1;
                let _ = eip;
            }
            s => panic!("{s:?}"),
        }
    }
    let dt = t.elapsed().as_secs_f64();
    eprintln!("api trap: {traps} round trips in {dt:.3}s = {:.2} us each", dt * 1e6 / traps as f64);
}
