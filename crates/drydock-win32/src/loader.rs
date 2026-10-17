//! Process creation and module loading.
//!
//! * Guest PE images (the executable and DLLs found through the VFS) are mapped with
//!   section protections, relocated when their preferred base is unavailable, bound
//!   against their imports, and initialised (TLS callbacks, DllMain) via explicit
//!   continuations on the guest thread.
//! * Rust-provided system DLLs are materialised as real guest images with a PE header
//!   and export directory whose functions are trap thunks. Guest code that walks export
//!   tables or calls GetProcAddress sees ordinary modules.
//!
//! The DLL search policy is explicit: names in the builtin table always resolve to the
//! builtin implementation; everything else is searched in the application directory,
//! the system directories, the current directory and PATH.

use crate::api::*;
use crate::diag::Severity;
use crate::objects::KObj;
use crate::sched::{Thread, ThreadState};
use crate::session::Session;
use crate::thunks::{THUNK_BYTES, THUNK_SIZE, ThunkInfo};
use crate::vfs::{self, GuestPath};
use drydock_core::cpu::{GDT_BASE, Regs};
use drydock_core::mem::*;
use drydock_core::pe::{self, Export, ExportTarget, Exports, ImportName, PeImage};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::BTreeMap;

pub const PEB_ADDR: u32 = 0x7ffd_f000;
pub const TEB_TOP: u32 = 0x7ffd_e000;
const BUILTIN_BASE: u32 = 0x7600_0000;
const DYNAMIC_THUNK_SLOTS: u32 = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Module {
    pub base: u32,
    pub size: u32,
    /// Base name as Windows reports it (e.g. "KERNEL32.dll").
    pub name: String,
    /// Full guest path.
    pub path: String,
    pub builtin: bool,
    pub exports: Exports,
    pub entry: u32,
    pub refs: u32,
    pub is_exe: bool,
    pub tls_index: Option<u32>,
    pub tls_template: Option<(u32, u32, u32)>,
    pub tls_callbacks: Vec<u32>,
    pub thread_calls: bool,
    pub sha256: Option<String>,
    /// Next free dynamic thunk slot (builtin modules) and its end.
    pub dyn_next: u32,
    pub dyn_end: u32,
    pub resources: Vec<pe::ResourceEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Modules {
    pub list: BTreeMap<u32, Module>,
    pub next_builtin: u32,
    /// Load order (bases), for init ordering and enumeration.
    pub order: Vec<u32>,
}

impl Modules {
    pub fn by_name(&self, name: &str) -> Option<&Module> {
        let want = basename(name).to_uppercase();
        let want_noext = want.trim_end_matches(".DLL").to_string();
        self.list.values().find(|m| {
            let n = m.name.to_uppercase();
            n == want || n.trim_end_matches(".DLL") == want_noext
        })
    }
    pub fn get(&self, base: u32) -> Option<&Module> {
        self.list.get(&base)
    }
    pub fn containing(&self, addr: u32) -> Option<&Module> {
        self.list
            .range(..=addr)
            .next_back()
            .map(|(_, m)| m)
            .filter(|m| addr < m.base + m.size)
    }
}

pub fn basename(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

/// Canonical builtin name for a requested DLL name, if builtin.
pub fn builtin_name(name: &str) -> Option<&'static str> {
    let b = basename(name).to_uppercase();
    let b = if b.contains('.') { b } else { format!("{b}.DLL") };
    crate::dll::BUILTINS
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(&b))
        .map(|d| d.name)
}

/// DLLs installed with the emulated Windows 2000 SP4 (with DirectX 7) profile. A
/// program that fails to load one of these hit missing platform functionality, not an
/// absent optional component.
#[rustfmt::skip]
const WINDOWS_SYSTEM_DLLS: &[&str] = &[
    "ADVAPI32.DLL", "AVIFIL32.DLL", "COMCTL32.DLL", "COMDLG32.DLL", "CRTDLL.DLL", "D3DIM.DLL", "D3DRM.DLL",
    "D3DXOF.DLL", "DDRAW.DLL", "DINPUT.DLL", "DPLAYX.DLL", "DSOUND.DLL", "DMUSIC.DLL", "DSETUP.DLL", "GDI32.DLL",
    "GLU32.DLL", "IMAGEHLP.DLL", "IMM32.DLL", "KERNEL32.DLL", "MFC42.DLL", "MPR.DLL", "MSACM32.DLL", "MSIMG32.DLL",
    "MSVCRT.DLL", "MSVCP60.DLL", "MSVFW32.DLL", "NETAPI32.DLL", "NTDLL.DLL", "OLE32.DLL", "OLEAUT32.DLL",
    "OPENGL32.DLL", "QUARTZ.DLL", "RICHED20.DLL", "RICHED32.DLL", "RPCRT4.DLL", "SETUPAPI.DLL", "SHELL32.DLL",
    "SHLWAPI.DLL", "URLMON.DLL", "USER32.DLL", "VERSION.DLL", "WININET.DLL", "WINMM.DLL", "WINSPOOL.DRV",
    "WS2_32.DLL", "WSOCK32.DLL",
];

/// Whether `name` is a DLL of the emulated Windows installation.
pub fn is_windows_system_dll(name: &str) -> bool {
    let b = basename(name).to_uppercase();
    let b = if b.contains('.') { b } else { format!("{b}.DLL") };
    WINDOWS_SYSTEM_DLLS.contains(&b.as_str())
}

// ---------------------------------------------------------------- builtin images

fn put_u16(v: &mut [u8], o: usize, x: u16) {
    v[o..o + 2].copy_from_slice(&x.to_le_bytes());
}
fn put_u32(v: &mut [u8], o: usize, x: u32) {
    v[o..o + 4].copy_from_slice(&x.to_le_bytes());
}

/// Materialise a builtin DLL as a guest image. Returns its base.
pub fn load_builtin(s: &mut Session, name: &'static str) -> Result<u32, String> {
    if let Some(m) = s.modules.by_name(name) {
        return Ok(m.base);
    }
    let def = crate::dll::BUILTINS
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| format!("no builtin {name}"))?;
    let exports: Vec<ExportDef> = def.parts.iter().flat_map(|p| p.iter().copied()).collect();
    let exports = &exports[..];
    // Assign ordinals: explicit ones first, then sequential after the highest.
    let mut next_ord = exports.iter().filter_map(|e| e.ordinal).max().unwrap_or(0) + 1;
    let mut ords: Vec<u16> = Vec::with_capacity(exports.len());
    for e in exports {
        ords.push(match e.ordinal {
            Some(o) => o,
            None => {
                let o = next_ord;
                next_ord += 1;
                o
            }
        });
    }
    let nfuncs = (*ords.iter().max().unwrap_or(&0)) as u32; // base = 1
    // Layout: [0x000 headers][0x1000 export dir + tables + names][thunks...]
    let exp_rva = 0x1000u32;
    let funcs_rva = exp_rva + 40;
    let names_rva = funcs_rva + nfuncs * 4;
    let nords_rva = names_rva + exports.len() as u32 * 4;
    let mut strings_rva = nords_rva + exports.len() as u32 * 2;
    let dllname_rva = strings_rva;
    strings_rva += name.len() as u32 + 1;
    let names_total: u32 = exports.iter().map(|e| e.name.len() as u32 + 1).sum();
    let exp_end = strings_rva + names_total;
    let thunk_rva = exp_end.div_ceil(PAGE_SIZE) * PAGE_SIZE;
    let nthunks = nfuncs + DYNAMIC_THUNK_SLOTS;
    let size = (thunk_rva + nthunks * THUNK_SIZE).div_ceil(0x10000) * 0x10000;

    let base = loop {
        let b = s.modules.next_builtin.max(BUILTIN_BASE);
        s.modules.next_builtin = b + size;
        if s.mem.is_free(b, size) {
            break b;
        }
    };
    let mut img = vec![0u8; size as usize];
    // DOS + PE headers.
    img[0..2].copy_from_slice(b"MZ");
    put_u32(&mut img, 0x3c, 0x40);
    img[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    put_u16(&mut img, coff, 0x14c);
    put_u16(&mut img, coff + 2, 1);
    put_u16(&mut img, coff + 16, 0xe0);
    put_u16(&mut img, coff + 18, 0x2102); // EXECUTABLE | 32BIT | DLL
    let opt = coff + 20;
    put_u16(&mut img, opt, 0x10b);
    put_u32(&mut img, opt + 4, size - thunk_rva); // SizeOfCode
    put_u32(&mut img, opt + 20, thunk_rva); // BaseOfCode
    put_u32(&mut img, opt + 28, base);
    put_u32(&mut img, opt + 32, PAGE_SIZE);
    put_u32(&mut img, opt + 36, 0x200);
    put_u16(&mut img, opt + 40, 4); // OS version 4.0
    put_u16(&mut img, opt + 48, 4);
    put_u32(&mut img, opt + 56, size);
    put_u32(&mut img, opt + 60, 0x1000);
    put_u16(&mut img, opt + 68, 2);
    put_u32(&mut img, opt + 92, 16);
    put_u32(&mut img, opt + 96, exp_rva);
    put_u32(&mut img, opt + 100, exp_end - exp_rva);
    let sec = opt + 0xe0;
    img[sec..sec + 5].copy_from_slice(b".text");
    put_u32(&mut img, sec + 8, size - 0x1000);
    put_u32(&mut img, sec + 12, 0x1000);
    put_u32(&mut img, sec + 16, size - 0x1000);
    put_u32(&mut img, sec + 20, 0x1000);
    put_u32(&mut img, sec + 36, 0x6000_0020);
    // Export directory.
    let e = exp_rva as usize;
    put_u32(&mut img, e + 12, dllname_rva);
    put_u32(&mut img, e + 16, 1);
    put_u32(&mut img, e + 20, nfuncs);
    put_u32(&mut img, e + 24, exports.len() as u32);
    put_u32(&mut img, e + 28, funcs_rva);
    put_u32(&mut img, e + 32, names_rva);
    put_u32(&mut img, e + 36, nords_rva);
    img[dllname_rva as usize..dllname_rva as usize + name.len()].copy_from_slice(name.as_bytes());
    let mut sorted: Vec<usize> = (0..exports.len()).collect();
    sorted.sort_by(|&a, &b| exports[a].name.as_bytes().cmp(exports[b].name.as_bytes()));
    let mut sp = strings_rva;
    let mut entries = Vec::new();
    for (k, &i) in sorted.iter().enumerate() {
        let ex = &exports[i];
        let ord = ords[i];
        let thunk = thunk_rva + (ord as u32 - 1) * THUNK_SIZE;
        put_u32(&mut img, (funcs_rva + (ord as u32 - 1) * 4) as usize, thunk);
        put_u32(&mut img, (names_rva + k as u32 * 4) as usize, sp);
        put_u16(&mut img, (nords_rva + k as u32 * 2) as usize, ord - 1);
        img[sp as usize..sp as usize + ex.name.len()].copy_from_slice(ex.name.as_bytes());
        sp += ex.name.len() as u32 + 1;
        s.thunks.insert(
            base + thunk,
            ThunkInfo::new(name.to_string(), ex.name.to_string(), ord, ex.conv, ex.handler),
        );
        entries.push(Export {
            ordinal: ord,
            name: Some(ex.name.to_string()),
            target: ExportTarget::Rva(thunk),
        });
    }
    for t in 0..nthunks {
        let o = (thunk_rva + t * THUNK_SIZE) as usize;
        img[o..o + 4].copy_from_slice(&THUNK_BYTES);
    }
    s.mem
        .reserve(Some(base), size, RegionKind::Image, PAGE_EXECUTE_READ, name)
        .map_err(|e| e.to_string())?;
    s.mem.commit(base, size, PAGE_READWRITE).map_err(|e| e.to_string())?;
    s.mem.write_force(base, &img).map_err(|e| e.to_string())?;
    s.mem
        .protect(base, thunk_rva, PAGE_READONLY)
        .map_err(|e| e.to_string())?;
    s.mem
        .protect(base + thunk_rva, size - thunk_rva, PAGE_EXECUTE_READ)
        .map_err(|e| e.to_string())?;
    let display = display_name(name);
    s.modules.list.insert(
        base,
        Module {
            base,
            size,
            name: display.clone(),
            path: format!("{}\\{}", s.config.windows.system_dir, display),
            builtin: true,
            exports: Exports {
                dll_name: name.to_string(),
                ordinal_base: 1,
                entries,
            },
            entry: 0,
            refs: 1,
            is_exe: false,
            tls_index: None,
            tls_template: None,
            tls_callbacks: Vec::new(),
            thread_calls: false,
            sha256: None,
            dyn_next: base + thunk_rva + nfuncs * THUNK_SIZE,
            dyn_end: base + thunk_rva + nthunks * THUNK_SIZE,
            resources: Vec::new(),
        },
    );
    s.modules.order.push(base);
    if let Some(f) = def.on_load {
        f(s);
    }
    Ok(base)
}

fn display_name(canon: &str) -> String {
    // "KERNEL32.DLL" -> "KERNEL32.dll", as Windows reports system modules.
    match canon.rsplit_once('.') {
        Some((b, e)) => format!("{b}.{}", e.to_lowercase()),
        None => canon.to_string(),
    }
}

/// Allocate a thunk in a builtin module for an import the builtin does not implement.
/// Calling it halts the session with an "unimplemented" diagnostic.
fn dynamic_stub(s: &mut Session, base: u32, name: &str, ordinal: u16) -> Result<u32, String> {
    let m = s.modules.list.get_mut(&base).unwrap();
    if m.dyn_next >= m.dyn_end {
        return Err(format!("{}: out of dynamic thunk slots", m.name));
    }
    let addr = m.dyn_next;
    m.dyn_next += THUNK_SIZE;
    let module = m.exports.dll_name.clone();
    s.thunks.insert(
        addr,
        ThunkInfo::new(module, name.to_string(), ordinal, Conv::Stdcall(0), None),
    );
    Ok(addr)
}

/// Allocate a non-exported thunk inside a builtin module (COM methods, window
/// procedures of system classes, internal callbacks).
pub fn internal_thunk(
    s: &mut Session,
    module: &'static str,
    name: &str,
    conv: Conv,
    handler: Option<Handler>,
) -> Result<u32, String> {
    let base = load_builtin(s, module)?;
    let addr = dynamic_stub(s, base, name, 0)?;
    let t = s.thunks.map.get_mut(&addr).unwrap();
    t.conv = conv;
    t.handler = handler;
    Ok(addr)
}

// ---------------------------------------------------------------- guest images

/// Resolve an export of a loaded module to an address (following forwarders).
pub fn resolve_export(s: &mut Session, base: u32, imp: &ImportName, depth: u32) -> Option<u32> {
    let m = s.modules.get(base)?;
    let e = match imp {
        ImportName::Name { name, .. } => m.exports.by_name(name),
        ImportName::Ordinal(o) => m.exports.by_ordinal(*o),
    }?
    .clone();
    match e.target {
        ExportTarget::Rva(r) => Some(base + r),
        ExportTarget::Forwarder(f) => {
            if depth > 8 {
                return None;
            }
            let (dll, func) = f.split_once('.')?;
            let tb = load_dll_by_name(s, &format!("{dll}.dll"), &mut Vec::new()).ok()?;
            let imp = if let Some(o) = func.strip_prefix('#') {
                ImportName::Ordinal(o.parse().ok()?)
            } else {
                ImportName::Name {
                    hint: 0,
                    name: func.to_string(),
                }
            };
            resolve_export(s, tb, &imp, depth + 1)
        }
    }
}

fn read_guest_file(s: &mut Session, p: &GuestPath) -> Result<Vec<u8>, String> {
    let (mut f, _) = s
        .vfs
        .open(p, true, false, vfs::Disposition::OpenExisting, 0)
        .map_err(|e| format!("open {}: {e:?}", p.display()))?;
    let n = s.vfs.file_size(&f) as usize;
    let mut buf = vec![0u8; n];
    s.vfs.read(&mut f, &mut buf).map_err(|e| format!("{e:?}"))?;
    Ok(buf)
}

/// Locate a DLL through the search policy. Returns the guest path.
pub fn search_dll(s: &Session, name: &str) -> Option<GuestPath> {
    let cwd = s.process.cwd.clone().unwrap_or(GuestPath {
        drive: 'C',
        comps: vec![],
    });
    let name = if basename(name).contains('.') {
        name.to_string()
    } else {
        format!("{name}.dll")
    };
    if name.contains('\\') || name.contains('/') || name.contains(':') {
        let p = vfs::normalize(&name, &cwd)?;
        return s.vfs.attributes(&p).ok().map(|_| p);
    }
    let mut dirs = Vec::new();
    if let Some(exe) = s.modules.list.values().find(|m| m.is_exe)
        && let Some(p) = vfs::normalize(&exe.path, &cwd)
        && let Some(d) = p.parent()
    {
        dirs.push(d);
    }
    for d in [&s.config.windows.system_dir, &s.config.windows.windows_dir] {
        if let Some(p) = vfs::normalize(d, &cwd) {
            dirs.push(p);
        }
    }
    dirs.push(cwd.clone());
    for d in dirs {
        let p = d.join(&name);
        if s.vfs.attributes(&p).is_ok() {
            return Some(p);
        }
    }
    None
}

/// Load a DLL (builtin or guest) by name, recording guest DLLs that need initialisation.
pub fn load_dll_by_name(s: &mut Session, name: &str, inits: &mut Vec<u32>) -> Result<u32, String> {
    if let Some(b) = builtin_name(name) {
        return load_builtin(s, b);
    }
    if let Some(m) = s.modules.by_name(name) {
        return Ok(m.base);
    }
    let path = search_dll(s, name).ok_or_else(|| format!("DLL not found: {name}"))?;
    if let Some(m) = s
        .modules
        .list
        .values()
        .find(|m| m.path.eq_ignore_ascii_case(&path.display()))
    {
        return Ok(m.base);
    }
    let data = read_guest_file(s, &path)?;
    load_pe(s, &path, &data, false, inits)
}

/// Map and bind a guest PE image. `inits` receives the bases of DLLs whose
/// initialisation (TLS callbacks + DllMain) must run, in dependency order.
pub fn load_pe(
    s: &mut Session,
    path: &GuestPath,
    file: &[u8],
    is_exe: bool,
    inits: &mut Vec<u32>,
) -> Result<u32, String> {
    let pe = PeImage::parse(file).map_err(|e| format!("{}: {e}", path.display()))?;
    if pe.machine != 0x14c {
        return Err(format!("{}: not an i386 image", path.display()));
    }
    let size = pe.size_of_image.div_ceil(PAGE_SIZE) * PAGE_SIZE;
    let mut img = pe.map_image(file);
    let has_relocs = pe.dir(pe::DIR_BASERELOC).rva != 0 && pe.characteristics & pe::IMAGE_FILE_RELOCS_STRIPPED == 0;
    let base = if s.mem.is_free(pe.image_base, size) && pe.image_base >= 0x10000 {
        pe.image_base
    } else if has_relocs {
        s.mem
            .find_free(size, ALLOC_GRANULARITY, 0x1000_0000, 0x7000_0000)
            .ok_or("no address space for image")?
    } else {
        return Err(format!(
            "{}: preferred base {:#x} unavailable and image has no relocations",
            path.display(),
            pe.image_base
        ));
    };
    if base != pe.image_base {
        pe.relocate(&mut img, base).map_err(|e| e.to_string())?;
        // Reflect the actual base in the mapped header, as the NT loader does.
        let opt = u32::from_le_bytes(img[0x3c..0x40].try_into().unwrap()) as usize + 24;
        img[opt + 28..opt + 32].copy_from_slice(&base.to_le_bytes());
    }
    s.mem
        .reserve(
            Some(base),
            size,
            RegionKind::Image,
            PAGE_EXECUTE_WRITECOPY,
            basename(&path.display()),
        )
        .map_err(|e| e.to_string())?;
    s.mem.commit(base, size, PAGE_READWRITE).map_err(|e| e.to_string())?;
    s.mem.write_force(base, &img).map_err(|e| e.to_string())?;
    let exports = pe.exports(&img).unwrap_or_default();
    let resources = pe.resources(&img).unwrap_or_default();
    let name = basename(&path.display()).to_string();
    let sha = hex::encode(sha2::Sha256::digest(file));
    s.modules.list.insert(
        base,
        Module {
            base,
            size,
            name: name.clone(),
            path: path.display(),
            builtin: false,
            exports,
            entry: if pe.entry_rva != 0 { base + pe.entry_rva } else { 0 },
            refs: 1,
            is_exe,
            tls_index: None,
            tls_template: None,
            tls_callbacks: Vec::new(),
            thread_calls: true,
            sha256: Some(sha),
            dyn_next: 0,
            dyn_end: 0,
            resources,
        },
    );
    s.modules.order.push(base);

    // Imports.
    let imports = pe.imports(&img).map_err(|e| format!("{name}: imports: {e}"))?;
    for dll in &imports {
        let dbase = load_dll_by_name(s, &dll.dll, inits).map_err(|e| format!("{name} imports {}: {e}", dll.dll))?;
        for ent in &dll.entries {
            let addr = match resolve_export(s, dbase, &ent.name, 0) {
                Some(a) => a,
                None => {
                    if s.modules.get(dbase).map(|m| m.builtin).unwrap_or(false) {
                        let (n, o) = match &ent.name {
                            ImportName::Name { name, .. } => (name.clone(), 0),
                            ImportName::Ordinal(o) => (format!("#{o}"), *o),
                        };
                        dynamic_stub(s, dbase, &n, o)?
                    } else {
                        return Err(format!("{name}: unresolved import {}!{}", dll.dll, ent.name));
                    }
                }
            };
            s.mem
                .write_force(base + ent.iat_rva, &addr.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
    }

    // TLS.
    if let Some(tls) = pe.tls(&img).map_err(|e| e.to_string())? {
        let idx = (0..64u32)
            .find(|i| s.process.tls_used & (1u64 << i) == 0)
            .ok_or("out of TLS slots")?;
        s.process.tls_used |= 1u64 << idx;
        if tls.index_va != 0 {
            s.mem
                .write_force(tls.index_va, &idx.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
        let mut cbs = Vec::new();
        if tls.callbacks_va != 0 {
            let mut p = tls.callbacks_va;
            loop {
                let cb = s.mem.read_u32(p).map_err(|e| e.to_string())?;
                if cb == 0 {
                    break;
                }
                cbs.push(cb);
                p += 4;
            }
        }
        let m = s.modules.list.get_mut(&base).unwrap();
        m.tls_index = Some(idx);
        m.tls_template = Some((tls.raw_start_va, tls.raw_end_va, tls.zero_fill));
        m.tls_callbacks = cbs;
    }

    // Section protections.
    s.mem
        .protect(base, pe.size_of_headers.max(1), PAGE_READONLY)
        .map_err(|e| e.to_string())?;
    for sec in &pe.sections {
        let vs = if sec.virtual_size != 0 {
            sec.virtual_size
        } else {
            sec.raw_size
        };
        if vs == 0 || sec.virtual_address >= size {
            continue;
        }
        let len = vs.min(size - sec.virtual_address);
        s.mem
            .protect(base + sec.virtual_address, len, sec.protect())
            .map_err(|e| e.to_string())?;
    }
    if !is_exe {
        inits.push(base);
    }
    s.event(
        Severity::Info,
        "module-load",
        format!("{} at {base:#x} ({size:#x} bytes)", path.display()),
    );
    Ok(base)
}

// ---------------------------------------------------------------- process & threads

fn write_unicode_string(s: &mut Session, at: u32, text: &str) -> Result<(), String> {
    let p = s.alloc_wstr(text).map_err(|e| e.to_string())?;
    let len = (text.encode_utf16().count() * 2) as u16;
    s.mem.write_u16(at, len).map_err(|e| e.to_string())?;
    s.mem.write_u16(at + 2, len + 2).map_err(|e| e.to_string())?;
    s.mem.write_u32(at + 4, p).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn init_process(s: &mut Session) -> Result<(), String> {
    s.mem
        .alloc(Some(GDT_BASE), PAGE_SIZE, RegionKind::System, PAGE_READWRITE, "gdt")
        .map_err(|e| e.to_string())?;
    s.cpu.setup_gdt(&mut s.mem)?;
    s.mem
        .alloc(Some(PEB_ADDR), PAGE_SIZE, RegionKind::System, PAGE_READWRITE, "peb")
        .map_err(|e| e.to_string())?;
    s.process.pid = 0x40;
    s.process.peb = PEB_ADDR;
    s.process.priority_class = 0x20;
    let (_, pobj) = s.objects.create_handle(KObj::Process { exit_code: None }, None);
    s.process.process_obj = pobj;

    let sys = s.heaps.create(&mut s.mem, 0, 0x10_0000, 0).map_err(|e| e.to_string())?;
    s.process.sys_heap = sys;
    let ph = s.heaps.create(&mut s.mem, 2, 0x10_0000, 0).map_err(|e| e.to_string())?;
    s.heaps.process_heap = ph;

    // Runtime-internal thunks.
    load_builtin(s, "DRYDOCK.DLL")?;
    s.thunks.callback_ret = s.thunks.find("DRYDOCK.DLL", "CallbackReturn").unwrap();
    s.thunks.thread_exit = s.thunks.find("DRYDOCK.DLL", "ThreadExit").unwrap();
    s.thunks.thread_start = s.thunks.find("DRYDOCK.DLL", "ThreadStart").unwrap();

    // Filesystem skeleton for the Windows profile.
    let now = s.filetime_now();
    let root = GuestPath {
        drive: 'C',
        comps: vec![],
    };
    for d in [
        s.config.windows.windows_dir.clone(),
        s.config.windows.system_dir.clone(),
        s.config.windows.temp_dir.clone(),
    ] {
        if let Some(p) = vfs::normalize(&d, &root) {
            let mut cur = GuestPath {
                drive: p.drive,
                comps: vec![],
            };
            for c in p.comps {
                cur = cur.join(&c);
                if !s.vfs.exists_dir(&cur) {
                    let _ = s.vfs.mkdir(&cur, now);
                }
            }
        }
    }

    // Executable.
    let exe_path = vfs::normalize(&s.config.exe, &root).ok_or("bad exe path")?;
    let exe_dir = exe_path.parent().unwrap_or(root.clone());
    s.process.cwd = Some(match &s.config.cwd {
        Some(c) => vfs::normalize(c, &root).ok_or("bad cwd")?,
        None => exe_dir.clone(),
    });
    let file = read_guest_file(s, &exe_path)?;
    let pe = PeImage::parse(&file).map_err(|e| e.to_string())?;
    if pe.is_dll() {
        return Err(format!("{} is a DLL, not an executable", exe_path.display()));
    }
    let mut inits = Vec::new();
    let base = load_pe(s, &exe_path, &file, true, &mut inits)?;
    s.process.image_base = base;
    s.process.exe_module = base;

    // Command line and environment.
    let exe_disp = exe_path.display();
    let mut cmd = if exe_disp.contains(' ') {
        format!("\"{exe_disp}\"")
    } else {
        exe_disp.clone()
    };
    if !s.config.args.is_empty() {
        cmd.push(' ');
        cmd.push_str(&s.config.args);
    }
    s.process.cmdline = cmd.clone();
    s.process.cmdline_a = s.alloc_str(&cmd).map_err(|e| e.to_string())?;
    s.process.cmdline_w = s.alloc_wstr(&cmd).map_err(|e| e.to_string())?;
    let w = s.config.windows.clone();
    let mut env: Vec<(String, String)> = vec![
        ("ComSpec".into(), format!("{}\\cmd.exe", w.system_dir)),
        ("COMPUTERNAME".into(), w.computer_name.clone()),
        (
            "OS".into(),
            if w.platform == 2 {
                "Windows_NT".into()
            } else {
                String::new()
            },
        ),
        ("Path".into(), format!("{};{}", w.system_dir, w.windows_dir)),
        ("SystemDrive".into(), "C:".into()),
        ("SystemRoot".into(), w.windows_dir.clone()),
        ("TEMP".into(), w.temp_dir.clone()),
        ("TMP".into(), w.temp_dir.clone()),
        ("USERNAME".into(), w.user_name.clone()),
        ("windir".into(), w.windows_dir.clone()),
    ];
    for (k, v) in s.config.env.clone() {
        env.retain(|(ek, _)| !ek.eq_ignore_ascii_case(&k));
        env.push((k, v));
    }
    env.sort_by_key(|(k, _)| k.to_uppercase());
    s.process.env = env.clone();
    let mut block_a = Vec::new();
    let mut block_w: Vec<u8> = Vec::new();
    for (k, v) in &env {
        let line = format!("{k}={v}");
        block_a.extend(crate::strings::encode_ansi(&line));
        block_a.push(0);
        block_w.extend(line.encode_utf16().flat_map(|c| c.to_le_bytes()));
        block_w.extend([0, 0]);
    }
    block_a.push(0);
    block_w.extend([0, 0]);
    let ea = s.sys_alloc(block_a.len() as u32).map_err(|e| e.to_string())?;
    s.mem.write(ea, &block_a).map_err(|e| e.to_string())?;
    let ew = s.sys_alloc(block_w.len() as u32).map_err(|e| e.to_string())?;
    s.mem.write(ew, &block_w).map_err(|e| e.to_string())?;
    s.process.env_a = ea;
    s.process.env_w = ew;

    // PEB and RTL_USER_PROCESS_PARAMETERS (the commonly inspected fields).
    let params = s.sys_alloc(0x2a0).map_err(|e| e.to_string())?;
    let cwd_disp = s.process.cwd.as_ref().unwrap().display();
    write_unicode_string(s, params + 0x24, &format!("{}\\", cwd_disp.trim_end_matches('\\')))?;
    write_unicode_string(s, params + 0x38, &exe_disp)?;
    write_unicode_string(s, params + 0x40, &cmd)?;
    s.mem.write_u32(params + 0x48, ew).map_err(|e| e.to_string())?;
    let m = |s: &mut Session, o: u32, v: u32| s.mem.write_u32(PEB_ADDR + o, v).map_err(|e| e.to_string());
    m(s, 0x08, base)?;
    m(s, 0x10, params)?;
    m(s, 0x18, ph)?;
    m(s, 0xa4, w.major)?;
    m(s, 0xa8, w.minor)?;
    s.mem
        .write_u16(PEB_ADDR + 0xac, w.build as u16)
        .map_err(|e| e.to_string())?;
    m(s, 0xb0, w.platform)?;
    m(s, 0x64, 1)?; // NumberOfProcessors

    // Main thread: process attach for static DLLs, exe TLS callbacks, then the entry point.
    let entry = s.modules.get(base).unwrap().entry;
    let reserve = pe.stack_reserve.max(0x10_0000);
    let mut calls = Vec::new();
    for &d in &inits {
        let m = s.modules.get(d).unwrap().clone();
        for cb in &m.tls_callbacks {
            calls.push(StartupCall {
                target: *cb,
                args: vec![d, 1, 1],
                check: false,
            });
        }
        if m.entry != 0 {
            calls.push(StartupCall {
                target: m.entry,
                args: vec![d, 1, 1],
                check: true,
            });
        }
    }
    for cb in s.modules.get(base).unwrap().tls_callbacks.clone() {
        calls.push(StartupCall {
            target: cb,
            args: vec![base, 1, 0],
            check: false,
        });
    }
    let tid = create_thread(s, entry, 0, reserve, false, false)?;
    s.sched.threads.get_mut(&tid).unwrap().regs.eip = s.thunks.thread_start;
    s.process.startup_pending = Some(StartupCont { calls, next: 0 });
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupCall {
    pub target: u32,
    pub args: Vec<u32>,
    /// DllMain: a FALSE return aborts the startup.
    pub check: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupCont {
    pub calls: Vec<StartupCall>,
    pub next: usize,
}

/// Create a guest thread whose first instruction is the ThreadStart thunk.
pub fn create_thread(
    s: &mut Session,
    start: u32,
    param: u32,
    stack_size: u32,
    suspended: bool,
    internal: bool,
) -> Result<u32, String> {
    let tid = s.sched.alloc_tid();
    let size = stack_size.max(0x10000).div_ceil(0x10000) * 0x10000;
    let stack = s
        .mem
        .alloc(None, size, RegionKind::Private, PAGE_READWRITE, "stack")
        .map_err(|e| e.to_string())?;
    let top = stack + size;
    // Find a TEB page.
    let mut teb = TEB_TOP;
    while !s.mem.is_free(teb, PAGE_SIZE) {
        teb -= PAGE_SIZE;
    }
    s.mem
        .alloc(Some(teb), PAGE_SIZE, RegionKind::System, PAGE_READWRITE, "teb")
        .map_err(|e| e.to_string())?;
    let w = |s: &mut Session, o: u32, v: u32| s.mem.write_u32(teb + o, v).map_err(|e| e.to_string());
    w(s, 0x00, 0xffff_ffff)?; // SEH chain end
    w(s, 0x04, top)?;
    w(s, 0x08, stack)?;
    w(s, 0x18, teb)?;
    w(s, 0x20, s.process.pid)?;
    w(s, 0x24, tid)?;
    w(s, 0x30, s.process.peb)?;
    // Implicit TLS: ThreadLocalStoragePointer -> array of per-module blocks.
    let tls_mods: Vec<(u32, (u32, u32, u32))> = s
        .modules
        .list
        .values()
        .filter_map(|m| Some((m.tls_index?, m.tls_template?)))
        .collect();
    if !tls_mods.is_empty() {
        let arr = s.sys_alloc(64 * 4).map_err(|e| e.to_string())?;
        for (idx, (start_va, end_va, zero)) in tls_mods {
            let n = end_va.saturating_sub(start_va);
            let blk = s.sys_alloc(n + zero + 4).map_err(|e| e.to_string())?;
            let data = s.mem.read_vec(start_va, n).map_err(|e| e.to_string())?;
            s.mem.write(blk, &data).map_err(|e| e.to_string())?;
            s.mem.write_u32(arr + idx * 4, blk).map_err(|e| e.to_string())?;
        }
        w(s, 0x2c, arr)?;
    }
    // Initial stack: [ThreadExit][start][param] as if ThreadStart(start, param) was called.
    let esp = top - 16;
    s.mem.write_u32(esp, s.thunks.thread_exit).map_err(|e| e.to_string())?;
    s.mem.write_u32(esp + 4, start).map_err(|e| e.to_string())?;
    s.mem.write_u32(esp + 8, param).map_err(|e| e.to_string())?;
    let obj = s.objects.create(KObj::Thread { tid }, None);
    s.objects.add_ref(obj);
    s.sched.threads.insert(
        tid,
        Thread {
            tid,
            obj,
            regs: Regs::initial(s.thunks.thread_start, esp, teb),
            state: ThreadState::Ready,
            frames: Vec::new(),
            teb,
            stack_region: stack,
            stack_top: top,
            priority: 0,
            suspend: if suspended { 1 } else { 0 },
            exit_code: None,
            start_addr: start,
            quantum_end: 0,
            internal,
        },
    );
    Ok(tid)
}

/// `ThreadStart(start, param)`: run pending initialisation callbacks, then the thread
/// routine; the routine's return value is returned to the ThreadExit thunk.
pub fn thread_start(s: &mut Session) -> ApiResult {
    let [start, param] = s.args::<2>()?;
    let mut st = if let Some(p) = s.process.startup_pending.take() {
        p
    } else {
        // DLL_THREAD_ATTACH for guest DLLs.
        let mut calls = Vec::new();
        let mods: Vec<Module> = s
            .modules
            .order
            .iter()
            .filter_map(|b| s.modules.get(*b).cloned())
            .filter(|m| !m.builtin)
            .collect();
        for m in mods {
            for cb in &m.tls_callbacks {
                calls.push(StartupCall {
                    target: *cb,
                    args: vec![m.base, 2, 0],
                    check: false,
                });
            }
            if !m.is_exe && m.entry != 0 && m.thread_calls {
                calls.push(StartupCall {
                    target: m.entry,
                    args: vec![m.base, 2, 0],
                    check: false,
                });
            }
        }
        StartupCont { calls, next: 0 }
    };
    st.calls.push(StartupCall {
        target: start,
        args: vec![param],
        check: false,
    });
    next_startup_call(st)
}

fn next_startup_call(mut st: StartupCont) -> ApiResult {
    let c = st.calls[st.next].clone();
    st.next += 1;
    Ok(ApiRet::call(
        c.target,
        c.args.into_iter().map(CallArg::Val).collect(),
        Cont::Startup(st),
    ))
}

pub fn resume_startup(s: &mut Session, st: StartupCont, v: Resume) -> ApiResult {
    let done = &st.calls[st.next - 1];
    if done.check && v.eax() == 0 {
        let name = s.modules.get(done.args[0]).map(|m| m.name.clone()).unwrap_or_default();
        return Err(ApiError::Fatal(format!(
            "DllMain of {name} failed process attach (STATUS_DLL_INIT_FAILED)"
        )));
    }
    if st.next >= st.calls.len() {
        return Ok(ApiRet::Ret(v.eax()));
    }
    next_startup_call(st)
}

pub fn persist_registry(s: &Session) -> std::io::Result<()> {
    if let Some(dir) = &s.vfs.persist_dir {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("registry.reg"), s.registry.export_reg())?;
    }
    Ok(())
}

impl Session {
    /// Write the file-system overlay and the registry to the persistent overlay
    /// directory, if the session has one.
    pub fn persist(&self) -> std::io::Result<()> {
        self.vfs.persist()?;
        persist_registry(self)
    }
}
