//! PE32 image parsing (headers, sections, imports, exports, relocations, TLS, resources).
//!
//! Parsing is purely structural and title-independent. Loading (mapping into the guest
//! address space, binding imports) lives in the Win32 layer, which owns module state.

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum PeError {
    #[error("not a PE image: {0}")]
    Format(&'static str),
    #[error("truncated image at offset {0:#x}")]
    Truncated(usize),
}

pub type PeResult<T> = Result<T, PeError>;

pub const IMAGE_FILE_DLL: u16 = 0x2000;
pub const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001;

pub const IMAGE_SCN_CNT_CODE: u32 = 0x20;
pub const IMAGE_SCN_CNT_UNINITIALIZED_DATA: u32 = 0x80;
pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
}

impl Section {
    /// Win32 page protection for this section's characteristics.
    pub fn protect(&self) -> u32 {
        use crate::mem::*;
        let c = self.characteristics;
        let x = c & IMAGE_SCN_MEM_EXECUTE != 0;
        let w = c & IMAGE_SCN_MEM_WRITE != 0;
        let r = c & IMAGE_SCN_MEM_READ != 0;
        match (x, w, r) {
            (true, true, _) => PAGE_EXECUTE_READWRITE,
            (true, false, _) => PAGE_EXECUTE_READ,
            (false, true, _) => PAGE_READWRITE,
            (false, false, true) => PAGE_READONLY,
            (false, false, false) => PAGE_READONLY,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DataDir {
    pub rva: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImportName {
    Name { hint: u16, name: String },
    Ordinal(u16),
}

impl std::fmt::Display for ImportName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportName::Name { name, .. } => write!(f, "{name}"),
            ImportName::Ordinal(o) => write!(f, "#{o}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEntry {
    pub name: ImportName,
    /// RVA of the IAT slot to bind.
    pub iat_rva: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportDll {
    pub dll: String,
    pub entries: Vec<ImportEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExportTarget {
    Rva(u32),
    Forwarder(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Export {
    pub ordinal: u16,
    pub name: Option<String>,
    pub target: ExportTarget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Exports {
    pub dll_name: String,
    pub ordinal_base: u32,
    pub entries: Vec<Export>,
}

impl Exports {
    pub fn by_name(&self, name: &str) -> Option<&Export> {
        self.entries.iter().find(|e| e.name.as_deref() == Some(name))
    }
    pub fn by_ordinal(&self, ord: u16) -> Option<&Export> {
        self.entries.iter().find(|e| e.ordinal == ord)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tls {
    pub raw_start_va: u32,
    pub raw_end_va: u32,
    pub index_va: u32,
    pub callbacks_va: u32,
    pub zero_fill: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeImage {
    pub machine: u16,
    pub characteristics: u16,
    pub timestamp: u32,
    pub image_base: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub entry_rva: u32,
    pub subsystem: u16,
    pub major_subsystem: u16,
    pub minor_subsystem: u16,
    pub stack_reserve: u32,
    pub stack_commit: u32,
    pub heap_reserve: u32,
    pub heap_commit: u32,
    pub dll_characteristics: u16,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub data_dirs: Vec<DataDir>,
    pub sections: Vec<Section>,
}

fn u16_at(b: &[u8], o: usize) -> PeResult<u16> {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or(PeError::Truncated(o))
}
fn u32_at(b: &[u8], o: usize) -> PeResult<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(PeError::Truncated(o))
}

pub const DIR_EXPORT: usize = 0;
pub const DIR_IMPORT: usize = 1;
pub const DIR_RESOURCE: usize = 2;
pub const DIR_BASERELOC: usize = 5;
pub const DIR_TLS: usize = 9;
pub const DIR_IAT: usize = 12;
pub const DIR_DELAY_IMPORT: usize = 13;

impl PeImage {
    pub fn parse(file: &[u8]) -> PeResult<PeImage> {
        if file.len() < 0x40 || &file[0..2] != b"MZ" {
            return Err(PeError::Format("missing MZ signature"));
        }
        let pe = u32_at(file, 0x3c)? as usize;
        if file.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Err(PeError::Format("missing PE signature"));
        }
        let coff = pe + 4;
        let machine = u16_at(file, coff)?;
        let nsections = u16_at(file, coff + 2)? as usize;
        let timestamp = u32_at(file, coff + 4)?;
        let opt_size = u16_at(file, coff + 16)? as usize;
        let characteristics = u16_at(file, coff + 18)?;
        let opt = coff + 20;
        let magic = u16_at(file, opt)?;
        if magic != 0x10b {
            return Err(PeError::Format("not a PE32 optional header"));
        }
        let entry_rva = u32_at(file, opt + 16)?;
        let image_base = u32_at(file, opt + 28)?;
        let section_alignment = u32_at(file, opt + 32)?;
        let file_alignment = u32_at(file, opt + 36)?;
        let major_subsystem = u16_at(file, opt + 48)?;
        let minor_subsystem = u16_at(file, opt + 50)?;
        let size_of_image = u32_at(file, opt + 56)?;
        let size_of_headers = u32_at(file, opt + 60)?;
        let subsystem = u16_at(file, opt + 68)?;
        let dll_characteristics = u16_at(file, opt + 70)?;
        let stack_reserve = u32_at(file, opt + 72)?;
        let stack_commit = u32_at(file, opt + 76)?;
        let heap_reserve = u32_at(file, opt + 80)?;
        let heap_commit = u32_at(file, opt + 84)?;
        let ndirs = (u32_at(file, opt + 92)? as usize).min(16);
        let mut data_dirs = Vec::with_capacity(16);
        for i in 0..ndirs {
            data_dirs.push(DataDir {
                rva: u32_at(file, opt + 96 + i * 8)?,
                size: u32_at(file, opt + 100 + i * 8)?,
            });
        }
        data_dirs.resize(16, DataDir::default());
        let mut sections = Vec::with_capacity(nsections);
        let sh = opt + opt_size;
        for i in 0..nsections {
            let o = sh + i * 40;
            let raw = file.get(o..o + 8).ok_or(PeError::Truncated(o))?;
            let name = String::from_utf8_lossy(raw).trim_end_matches('\0').to_string();
            sections.push(Section {
                name,
                virtual_size: u32_at(file, o + 8)?,
                virtual_address: u32_at(file, o + 12)?,
                raw_size: u32_at(file, o + 16)?,
                raw_offset: u32_at(file, o + 20)?,
                characteristics: u32_at(file, o + 36)?,
            });
        }
        Ok(PeImage {
            machine,
            characteristics,
            timestamp,
            image_base,
            size_of_image,
            size_of_headers,
            entry_rva,
            subsystem,
            major_subsystem,
            minor_subsystem,
            stack_reserve,
            stack_commit,
            heap_reserve,
            heap_commit,
            dll_characteristics,
            section_alignment,
            file_alignment,
            data_dirs,
            sections,
        })
    }

    pub fn is_dll(&self) -> bool {
        self.characteristics & IMAGE_FILE_DLL != 0
    }

    pub fn dir(&self, i: usize) -> DataDir {
        self.data_dirs[i]
    }

    /// Lay out the image as it appears in memory (headers + sections, zero-filled).
    pub fn map_image(&self, file: &[u8]) -> Vec<u8> {
        let mut img = vec![0u8; self.size_of_image as usize];
        let hdr = (self.size_of_headers as usize).min(file.len()).min(img.len());
        img[..hdr].copy_from_slice(&file[..hdr]);
        for s in &self.sections {
            let va = s.virtual_address as usize;
            if va >= img.len() {
                continue;
            }
            // Raw data beyond VirtualSize is not mapped (when VirtualSize is set).
            let mut n = s.raw_size as usize;
            if s.virtual_size != 0 {
                n = n.min(s.virtual_size as usize);
            }
            let off = s.raw_offset as usize;
            if off >= file.len() {
                continue;
            }
            let n = n.min(file.len() - off).min(img.len() - va);
            img[va..va + n].copy_from_slice(&file[off..off + n]);
        }
        img
    }

    /// Imports, read from a mapped image (RVAs index the mapped layout).
    pub fn imports(&self, img: &[u8]) -> PeResult<Vec<ImportDll>> {
        let d = self.dir(DIR_IMPORT);
        let mut out = Vec::new();
        if d.rva == 0 {
            return Ok(out);
        }
        let mut desc = d.rva as usize;
        loop {
            let ilt = u32_at(img, desc)?;
            let name_rva = u32_at(img, desc + 12)?;
            let iat = u32_at(img, desc + 16)?;
            if ilt == 0 && name_rva == 0 && iat == 0 {
                break;
            }
            let dll = cstr_at(img, name_rva as usize)?;
            // Use the ILT if present, otherwise the IAT (unbound images).
            let mut thunk = if ilt != 0 { ilt } else { iat } as usize;
            let mut slot = iat as usize;
            let mut entries = Vec::new();
            loop {
                let v = u32_at(img, thunk)?;
                if v == 0 {
                    break;
                }
                let name = if v & 0x8000_0000 != 0 {
                    ImportName::Ordinal(v as u16)
                } else {
                    let hint = u16_at(img, v as usize)?;
                    ImportName::Name {
                        hint,
                        name: cstr_at(img, v as usize + 2)?,
                    }
                };
                entries.push(ImportEntry {
                    name,
                    iat_rva: slot as u32,
                });
                thunk += 4;
                slot += 4;
            }
            out.push(ImportDll { dll, entries });
            desc += 20;
        }
        Ok(out)
    }

    pub fn exports(&self, img: &[u8]) -> PeResult<Exports> {
        let d = self.dir(DIR_EXPORT);
        if d.rva == 0 {
            return Ok(Exports::default());
        }
        let e = d.rva as usize;
        let dll_name = cstr_at(img, u32_at(img, e + 12)? as usize).unwrap_or_default();
        let ordinal_base = u32_at(img, e + 16)?;
        let nfuncs = u32_at(img, e + 20)? as usize;
        let nnames = u32_at(img, e + 24)? as usize;
        let funcs = u32_at(img, e + 28)? as usize;
        let names = u32_at(img, e + 32)? as usize;
        let ords = u32_at(img, e + 36)? as usize;
        let mut name_of = vec![None; nfuncs];
        for i in 0..nnames {
            let idx = u16_at(img, ords + i * 2)? as usize;
            let n = cstr_at(img, u32_at(img, names + i * 4)? as usize)?;
            if idx < nfuncs {
                name_of[idx] = Some(n);
            }
        }
        let mut entries = Vec::new();
        for (i, name) in name_of.into_iter().enumerate() {
            let rva = u32_at(img, funcs + i * 4)?;
            if rva == 0 {
                continue;
            }
            let target = if rva >= d.rva && rva < d.rva + d.size {
                ExportTarget::Forwarder(cstr_at(img, rva as usize)?)
            } else {
                ExportTarget::Rva(rva)
            };
            entries.push(Export {
                ordinal: (ordinal_base as usize + i) as u16,
                name,
                target,
            });
        }
        Ok(Exports {
            dll_name,
            ordinal_base,
            entries,
        })
    }

    /// Apply base relocations to a mapped image loaded at `new_base`.
    /// Returns the number of fixups applied.
    pub fn relocate(&self, img: &mut [u8], new_base: u32) -> PeResult<usize> {
        let delta = new_base.wrapping_sub(self.image_base);
        let d = self.dir(DIR_BASERELOC);
        if delta == 0 || d.rva == 0 {
            return Ok(0);
        }
        let mut p = d.rva as usize;
        let end = (d.rva + d.size) as usize;
        let mut count = 0;
        while p + 8 <= end {
            let page = u32_at(img, p)? as usize;
            let block = u32_at(img, p + 4)? as usize;
            if block < 8 {
                break;
            }
            for i in 0..(block - 8) / 2 {
                let e = u16_at(img, p + 8 + i * 2)?;
                let ty = e >> 12;
                let off = page + (e & 0xfff) as usize;
                match ty {
                    0 => {}
                    3 => {
                        let v = u32_at(img, off)?.wrapping_add(delta);
                        img[off..off + 4].copy_from_slice(&v.to_le_bytes());
                        count += 1;
                    }
                    2 => {
                        let v = u16_at(img, off)?.wrapping_add((delta >> 16) as u16);
                        img[off..off + 2].copy_from_slice(&v.to_le_bytes());
                        count += 1;
                    }
                    1 => {
                        let v = u16_at(img, off)?.wrapping_add(delta as u16);
                        img[off..off + 2].copy_from_slice(&v.to_le_bytes());
                        count += 1;
                    }
                    _ => return Err(PeError::Format("unsupported relocation type")),
                }
            }
            p += block;
        }
        Ok(count)
    }

    pub fn tls(&self, img: &[u8]) -> PeResult<Option<Tls>> {
        let d = self.dir(DIR_TLS);
        if d.rva == 0 {
            return Ok(None);
        }
        let o = d.rva as usize;
        Ok(Some(Tls {
            raw_start_va: u32_at(img, o)?,
            raw_end_va: u32_at(img, o + 4)?,
            index_va: u32_at(img, o + 8)?,
            callbacks_va: u32_at(img, o + 12)?,
            zero_fill: u32_at(img, o + 16)?,
        }))
    }
}

pub fn cstr_at(b: &[u8], o: usize) -> PeResult<String> {
    let s = b.get(o..).ok_or(PeError::Truncated(o))?;
    let n = s.iter().position(|&c| c == 0).ok_or(PeError::Truncated(o))?;
    Ok(String::from_utf8_lossy(&s[..n]).into_owned())
}

/// Resource directory types.
pub const RT_CURSOR: u32 = 1;
pub const RT_BITMAP: u32 = 2;
pub const RT_ICON: u32 = 3;
pub const RT_MENU: u32 = 4;
pub const RT_DIALOG: u32 = 5;
pub const RT_STRING: u32 = 6;
pub const RT_GROUP_CURSOR: u32 = 12;
pub const RT_GROUP_ICON: u32 = 14;
pub const RT_VERSION: u32 = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResId {
    Id(u32),
    Name(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEntry {
    pub ty: ResId,
    pub name: ResId,
    pub lang: u32,
    /// RVA and size of the resource data within the mapped image.
    pub data_rva: u32,
    pub size: u32,
}

impl PeImage {
    /// Flatten the three-level resource tree (type / name / language).
    pub fn resources(&self, img: &[u8]) -> PeResult<Vec<ResourceEntry>> {
        let d = self.dir(DIR_RESOURCE);
        let mut out = Vec::new();
        if d.rva == 0 {
            return Ok(out);
        }
        let root = d.rva as usize;
        let read_dir = |off: usize| -> PeResult<Vec<(ResId, u32)>> {
            let nnamed = u16_at(img, root + off + 12)? as usize;
            let nid = u16_at(img, root + off + 14)? as usize;
            let mut v = Vec::new();
            for i in 0..nnamed + nid {
                let e = root + off + 16 + i * 8;
                let name = u32_at(img, e)?;
                let target = u32_at(img, e + 4)?;
                let id = if name & 0x8000_0000 != 0 {
                    let so = root + (name & 0x7fff_ffff) as usize;
                    let len = u16_at(img, so)? as usize;
                    let mut s = Vec::with_capacity(len);
                    for k in 0..len {
                        s.push(u16_at(img, so + 2 + k * 2)?);
                    }
                    ResId::Name(String::from_utf16_lossy(&s).to_uppercase())
                } else {
                    ResId::Id(name)
                };
                v.push((id, target));
            }
            Ok(v)
        };
        for (ty, t1) in read_dir(0)? {
            if t1 & 0x8000_0000 == 0 {
                continue;
            }
            for (name, t2) in read_dir((t1 & 0x7fff_ffff) as usize)? {
                if t2 & 0x8000_0000 == 0 {
                    continue;
                }
                for (lang, t3) in read_dir((t2 & 0x7fff_ffff) as usize)? {
                    if t3 & 0x8000_0000 != 0 {
                        continue;
                    }
                    let de = root + t3 as usize;
                    out.push(ResourceEntry {
                        ty: ty.clone(),
                        name: name.clone(),
                        lang: match lang {
                            ResId::Id(l) => l,
                            ResId::Name(_) => 0,
                        },
                        data_rva: u32_at(img, de)?,
                        size: u32_at(img, de + 4)?,
                    });
                }
            }
        }
        Ok(out)
    }
}
