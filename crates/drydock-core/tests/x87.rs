//! The vendored Unicorn computes common x87 operations with host doubles
//! (vendor/patches/0005). Run the same randomised x87 program with those fast paths on
//! and off: results and status words must be bit-identical.
use drydock_core::cpu::{Cpu, GDT_BASE, Regs, Stop, set_fast_fp};
use drydock_core::mem::*;
use iced_x86::code_asm::*;

const CASES: u32 = 4000;
const IN: u32 = 0x42_0000;
const OUT: u32 = 0x50_0000;
const CW: u32 = 0x41_0000;
const SLOTS: u32 = 16 + 9 * 16;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn x80_from_f64(d: f64) -> [u8; 10] {
    let b = d.to_bits();
    let (sign, e, frac) = ((b >> 63) as u16, ((b >> 52) & 0x7ff) as i32, b & ((1 << 52) - 1));
    let (exp, mant) = if e == 0 && frac == 0 {
        (0u16, 0u64)
    } else if e == 0x7ff {
        (0x7fff, (1 << 63) | (frac << 11))
    } else if e == 0 {
        let lz = frac.leading_zeros() as i32 - 11;
        ((1 - 1023 + 0x3fff - lz) as u16, (frac << (11 + lz)) | 0)
    } else {
        ((e - 1023 + 0x3fff) as u16, (1 << 63) | (frac << 11))
    };
    let mut out = [0u8; 10];
    out[..8].copy_from_slice(&mant.to_le_bytes());
    out[8..].copy_from_slice(&(exp | sign << 15).to_le_bytes());
    out
}

fn random_operand(r: &mut Rng) -> [u8; 10] {
    match r.next() % 8 {
        // Typical values: moderate exponent, exactly a double.
        0..=3 => {
            let e = 1023 - 20 + (r.next() % 40) as u64;
            x80_from_f64(f64::from_bits(
                (r.next() & 1) << 63 | e << 52 | (r.next() & ((1 << 52) - 1)),
            ))
        }
        // Wide exponent range, exactly a double (including subnormal and special).
        4 => x80_from_f64(f64::from_bits(r.next())),
        // Short mantissas, so results are often exact.
        5 => x80_from_f64(((r.next() % 2001) as f64 - 1000.0) * [0.5, 1.0, 0.25, 8.0][(r.next() % 4) as usize]),
        // Full 64-bit mantissa.
        6 => {
            let mut out = [0u8; 10];
            out[..8].copy_from_slice(&(r.next() | 1 << 63).to_le_bytes());
            let exp = 0x3fff - 30 + (r.next() % 60) as u16;
            out[8..].copy_from_slice(&(exp | ((r.next() & 1) as u16) << 15).to_le_bytes());
            out
        }
        _ => x80_from_f64([0.0, -0.0, 1.0, -1.0][(r.next() % 4) as usize]),
    }
}

fn run(fast: bool, input: &[u8]) -> Vec<u8> {
    set_fast_fp(fast);
    let mut a = CodeAssembler::new(32).unwrap();
    let mut l = a.create_label();
    a.fldcw(word_ptr(CW)).unwrap();
    a.mov(esi, IN).unwrap();
    a.mov(edi, OUT).unwrap();
    a.mov(ecx, CASES).unwrap();
    a.set_label(&mut l).unwrap();
    macro_rules! slot {
        () => {
            a.fstp(tbyte_ptr(edi)).unwrap();
            a.fnstsw(ax).unwrap();
            a.mov(word_ptr(edi + 10), ax).unwrap();
            a.fnclex().unwrap();
            a.add(edi, 16).unwrap();
        };
    }
    macro_rules! two {
        ($op:ident) => {
            a.fld(tbyte_ptr(esi)).unwrap();
            a.fld(tbyte_ptr(esi + 16)).unwrap();
            a.$op(st1, st0).unwrap();
            slot!();
        };
    }
    two!(faddp);
    two!(fsubp);
    two!(fmulp);
    two!(fdivp);
    two!(fsubrp);
    two!(fdivrp);
    for op in 0..4 {
        a.fld(tbyte_ptr(esi)).unwrap();
        match op {
            0 => a.fadd(qword_ptr(esi + 32)),
            1 => a.fmul(qword_ptr(esi + 32)),
            2 => a.fsub(qword_ptr(esi + 32)),
            _ => a.fdiv(qword_ptr(esi + 32)),
        }
        .unwrap();
        slot!();
    }
    // Store as single and double, load a single.
    a.fld(tbyte_ptr(esi)).unwrap();
    a.fstp(dword_ptr(edi)).unwrap();
    a.fld(tbyte_ptr(esi + 16)).unwrap();
    a.fstp(qword_ptr(edi + 4)).unwrap();
    a.fnstsw(ax).unwrap();
    a.mov(word_ptr(edi + 12), ax).unwrap();
    a.fnclex().unwrap();
    a.add(edi, 16).unwrap();
    a.fld(dword_ptr(esi + 40)).unwrap();
    slot!();
    a.add(esi, 48).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(l).unwrap();
    a.int(0x2e).unwrap();
    let code = a.assemble(0x40_1000).unwrap();

    let mut mem = AddressSpace::new();
    mem.alloc(
        Some(0x40_0000),
        0x10000,
        RegionKind::Image,
        PAGE_EXECUTE_READWRITE,
        "code",
    )
    .unwrap();
    mem.alloc(Some(0xf_0000), 0x10000, RegionKind::Private, PAGE_READWRITE, "stack")
        .unwrap();
    mem.alloc(Some(CW), 0x1000, RegionKind::Private, PAGE_READWRITE, "cw")
        .unwrap();
    mem.alloc(Some(IN), 0x40000, RegionKind::Private, PAGE_READWRITE, "in")
        .unwrap();
    mem.alloc(Some(OUT), 0x100000, RegionKind::Private, PAGE_READWRITE, "out")
        .unwrap();
    mem.alloc(Some(GDT_BASE), 0x1000, RegionKind::System, PAGE_READWRITE, "gdt")
        .unwrap();
    mem.write_force(0x40_1000, &code).unwrap();
    mem.write_force(CW, &0x27fu16.to_le_bytes()).unwrap();
    mem.write_force(IN, input).unwrap();
    let mut cpu = Cpu::new().unwrap();
    cpu.setup_gdt(&mut mem).unwrap();
    cpu.write_regs(&mut mem, &Regs::initial(0x40_1000, 0xf_fff0, 0))
        .unwrap();
    loop {
        match cpu.run_slice(&mut mem, 1_000_000, 0).unwrap() {
            Stop::Budget => continue,
            Stop::ApiTrap { .. } => break,
            s => panic!("{s:?}"),
        }
    }
    mem.read_vec(OUT, CASES * SLOTS).unwrap()
}

#[test]
fn x87_fast_paths_match_softfloat() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut input = Vec::new();
    for _ in 0..CASES {
        let (a, b) = (random_operand(&mut r), random_operand(&mut r));
        let q = f64::from_bits(r.next() >> 1 & !(0x7ffu64 << 52) | (1023 - 8 + r.next() % 16) << 52);
        let f = f32::from_bits(r.next() as u32);
        let mut rec = [0u8; 48];
        rec[..10].copy_from_slice(&a);
        rec[16..26].copy_from_slice(&b);
        rec[32..40].copy_from_slice(&q.to_le_bytes());
        rec[40..44].copy_from_slice(&f.to_bits().to_le_bytes());
        input.extend_from_slice(&rec);
    }
    let slow = run(false, &input);
    let fast = run(true, &input);
    set_fast_fp(true);
    assert_eq!(slow.len(), fast.len());
    for (i, (s, f)) in slow.chunks(16).zip(fast.chunks(16)).enumerate() {
        assert_eq!(s, f, "slot {i} (case {}, op {})", i / 13, i % 13);
    }
}
