//! Builds small PE32 executables for Drydock's own tests.
//!
//! Every fixture is authored here from scratch (x86 code through iced's assembler),
//! so tests never depend on third-party binaries. Images have a fixed base and no
//! relocations, an import table, an initialised data section and one code section.

use iced_x86::IcedError;
use iced_x86::code_asm::CodeAssembler;
use std::collections::HashMap;

pub const IMAGE_BASE: u32 = 0x40_0000;
const SECT_ALIGN: u32 = 0x1000;
const FILE_ALIGN: u32 = 0x200;
const HEADERS_SIZE: u32 = 0x400;

/// Subsystem of the generated image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    Gui = 2,
    Console = 3,
}

/// Addresses known before the code is assembled.
pub struct Layout {
    iat: HashMap<(String, String), u32>,
    /// Virtual address of the start of the data section.
    pub data: u32,
    /// Virtual address the code section is assembled at.
    pub code: u32,
}

impl Layout {
    /// Address of the IAT slot for `dll!func` (call through it with `call dword ptr [slot]`).
    pub fn import(&self, dll: &str, func: &str) -> u32 {
        *self
            .iat
            .get(&(dll.to_ascii_uppercase(), func.to_string()))
            .unwrap_or_else(|| panic!("{dll}!{func} was not declared as an import"))
    }
}

/// Description of a fixture executable.
pub struct PeBuilder {
    imports: Vec<(String, Vec<String>)>,
    data: Vec<u8>,
    bss: u32,
    subsystem: Subsystem,
}

impl Default for PeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn align(v: u32, a: u32) -> u32 {
    v.div_ceil(a) * a
}

impl PeBuilder {
    pub fn new() -> Self {
        PeBuilder {
            imports: Vec::new(),
            data: Vec::new(),
            bss: 0,
            subsystem: Subsystem::Console,
        }
    }

    pub fn subsystem(mut self, s: Subsystem) -> Self {
        self.subsystem = s;
        self
    }

    /// Declare imported functions of a DLL (by name).
    pub fn import(mut self, dll: &str, funcs: &[&str]) -> Self {
        let dll = dll.to_ascii_uppercase();
        match self.imports.iter_mut().find(|(d, _)| *d == dll) {
            Some((_, f)) => f.extend(funcs.iter().map(|s| s.to_string())),
            None => self.imports.push((dll, funcs.iter().map(|s| s.to_string()).collect())),
        }
        self
    }

    /// Initialised data placed at the start of the data section ([`Layout::data`]).
    pub fn data(mut self, bytes: &[u8]) -> Self {
        self.data = bytes.to_vec();
        self
    }

    /// Zero-initialised space reserved after the initialised data.
    pub fn bss(mut self, bytes: u32) -> Self {
        self.bss = bytes;
        self
    }

    /// Assemble the code (entry point at its first instruction) and emit the image.
    pub fn build(self, code: impl FnOnce(&mut CodeAssembler, &Layout) -> Result<(), IcedError>) -> Vec<u8> {
        // .idata at the first section RVA.
        let idata_rva = SECT_ALIGN;
        let (idata, iat_slots, iat_range) = self.build_idata(idata_rva);
        let data_rva = align(idata_rva + idata.len().max(1) as u32, SECT_ALIGN);
        let data_vsize = (self.data.len() as u32 + self.bss).max(4);
        let code_rva = align(data_rva + data_vsize, SECT_ALIGN);
        let layout = Layout {
            iat: iat_slots,
            data: IMAGE_BASE + data_rva,
            code: IMAGE_BASE + code_rva,
        };

        let mut a = CodeAssembler::new(32).expect("32-bit assembler");
        code(&mut a, &layout).expect("fixture code assembles");
        let text = a.assemble(layout.code as u64).expect("fixture code encodes");

        let sections: [(&[u8; 8], u32, &[u8], u32, u32); 3] = [
            (b".idata\0\0", idata_rva, &idata, idata.len() as u32, 0xC000_0040),
            (b".data\0\0\0", data_rva, &self.data, data_vsize, 0xC000_0040),
            (b".text\0\0\0", code_rva, &text, text.len() as u32, 0x6000_0020),
        ];
        let image_size = align(code_rva + text.len().max(1) as u32, SECT_ALIGN);

        let mut out = vec![0u8; HEADERS_SIZE as usize];
        // DOS header.
        out[0..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let mut h = Vec::new();
        h.extend(b"PE\0\0");
        // COFF header: i386, 3 sections, optional header size, flags
        // (relocs stripped | executable | 32-bit machine).
        h.extend(0x14cu16.to_le_bytes());
        h.extend(3u16.to_le_bytes());
        h.extend([0u8; 12]);
        h.extend(0xE0u16.to_le_bytes());
        h.extend(0x0103u16.to_le_bytes());
        // Optional header (PE32).
        h.extend(0x10bu16.to_le_bytes());
        h.extend([6u8, 0]);
        h.extend((text.len() as u32).to_le_bytes());
        h.extend((idata.len() as u32 + data_vsize).to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.extend(code_rva.to_le_bytes()); // entry point
        h.extend(code_rva.to_le_bytes()); // base of code
        h.extend(data_rva.to_le_bytes()); // base of data
        h.extend(IMAGE_BASE.to_le_bytes());
        h.extend(SECT_ALIGN.to_le_bytes());
        h.extend(FILE_ALIGN.to_le_bytes());
        h.extend(4u16.to_le_bytes()); // OS version
        h.extend(0u16.to_le_bytes());
        h.extend(0u32.to_le_bytes()); // image version
        h.extend(4u16.to_le_bytes()); // subsystem version
        h.extend(0u16.to_le_bytes());
        h.extend(0u32.to_le_bytes()); // win32 version
        h.extend(image_size.to_le_bytes());
        h.extend(HEADERS_SIZE.to_le_bytes());
        h.extend(0u32.to_le_bytes()); // checksum
        h.extend((self.subsystem as u16).to_le_bytes());
        h.extend(0u16.to_le_bytes()); // DLL characteristics
        for v in [0x10_0000u32, 0x1000, 0x10_0000, 0x1000, 0, 16] {
            h.extend(v.to_le_bytes());
        }
        let mut dirs = [(0u32, 0u32); 16];
        if !self.imports.is_empty() {
            dirs[1] = (idata_rva, (self.imports.len() as u32 + 1) * 20);
            dirs[12] = iat_range;
        }
        for (rva, size) in dirs {
            h.extend(rva.to_le_bytes());
            h.extend(size.to_le_bytes());
        }
        // Section table, raw data laid out in order.
        let mut raw_ptr = HEADERS_SIZE;
        let mut bodies = Vec::new();
        for (name, rva, body, vsize, chars) in sections {
            let raw_size = align(body.len() as u32, FILE_ALIGN);
            h.extend(name);
            h.extend(vsize.max(1).to_le_bytes());
            h.extend(rva.to_le_bytes());
            h.extend(raw_size.to_le_bytes());
            h.extend((if raw_size == 0 { 0 } else { raw_ptr }).to_le_bytes());
            h.extend([0u8; 12]);
            h.extend(chars.to_le_bytes());
            let mut b = body.to_vec();
            b.resize(raw_size as usize, 0);
            bodies.push(b);
            raw_ptr += raw_size;
        }
        assert!(0x40 + h.len() <= HEADERS_SIZE as usize, "headers overflow");
        out[0x40..0x40 + h.len()].copy_from_slice(&h);
        for b in bodies {
            out.extend(b);
        }
        out
    }

    /// Import directory, lookup tables, IAT, hint/name entries and DLL names.
    #[allow(clippy::type_complexity)]
    fn build_idata(&self, rva: u32) -> (Vec<u8>, HashMap<(String, String), u32>, (u32, u32)) {
        let mut slots = HashMap::new();
        if self.imports.is_empty() {
            return (Vec::new(), slots, (0, 0));
        }
        let ndesc = self.imports.len() + 1;
        let thunk_words: usize = self.imports.iter().map(|(_, f)| f.len() + 1).sum();
        let ilt_off = ndesc * 20;
        let iat_off = ilt_off + thunk_words * 4;
        let names_off = iat_off + thunk_words * 4;
        let mut names = Vec::new();
        let mut buf = vec![0u8; names_off];
        let mut word = 0usize;
        for (i, (dll, funcs)) in self.imports.iter().enumerate() {
            let d = i * 20;
            let ilt = (rva as usize + ilt_off + word * 4) as u32;
            let iat = (rva as usize + iat_off + word * 4) as u32;
            for f in funcs {
                let hint_rva = (rva as usize + names_off + names.len()) as u32;
                names.extend(0u16.to_le_bytes());
                names.extend(f.as_bytes());
                names.push(0);
                if names.len() % 2 == 1 {
                    names.push(0);
                }
                let o = word * 4;
                buf[ilt_off + o..ilt_off + o + 4].copy_from_slice(&hint_rva.to_le_bytes());
                buf[iat_off + o..iat_off + o + 4].copy_from_slice(&hint_rva.to_le_bytes());
                slots.insert((dll.clone(), f.clone()), IMAGE_BASE + rva + (iat_off + o) as u32);
                word += 1;
            }
            word += 1; // null terminator
            let name_rva = (rva as usize + names_off + names.len()) as u32;
            names.extend(dll.as_bytes());
            names.push(0);
            buf[d..d + 4].copy_from_slice(&ilt.to_le_bytes());
            buf[d + 12..d + 16].copy_from_slice(&name_rva.to_le_bytes());
            buf[d + 16..d + 20].copy_from_slice(&iat.to_le_bytes());
        }
        buf.extend(names);
        let iat_range = (rva + iat_off as u32, (thunk_words * 4) as u32);
        (buf, slots, iat_range)
    }
}

/// A console program that immediately calls `ExitProcess(code)`.
pub fn exit_with(code: u32) -> Vec<u8> {
    use iced_x86::code_asm::*;
    PeBuilder::new().import("KERNEL32.DLL", &["ExitProcess"]).build(|a, l| {
        a.push(code)?;
        a.call(dword_ptr(l.import("KERNEL32.DLL", "ExitProcess")))?;
        a.int3()
    })
}
