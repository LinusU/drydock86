//! Emulated registry: an in-memory tree seeded from user-supplied `.reg` files.
//!
//! No application-specific keys are built in. The session's registry is part of the
//! checkpointed state and can be persisted (as a `.reg` file) alongside the overlay.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const HKEY_CLASSES_ROOT: u32 = 0x8000_0000;
pub const HKEY_CURRENT_USER: u32 = 0x8000_0001;
pub const HKEY_LOCAL_MACHINE: u32 = 0x8000_0002;
pub const HKEY_USERS: u32 = 0x8000_0003;
pub const HKEY_CURRENT_CONFIG: u32 = 0x8000_0005;

pub const REG_NONE: u32 = 0;
pub const REG_SZ: u32 = 1;
pub const REG_EXPAND_SZ: u32 = 2;
pub const REG_BINARY: u32 = 3;
pub const REG_DWORD: u32 = 4;
pub const REG_MULTI_SZ: u32 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegValue {
    pub name: String,
    pub ty: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegKey {
    pub name: String,
    pub values: BTreeMap<String, RegValue>,
    pub subkeys: BTreeMap<String, RegKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub roots: BTreeMap<u32, RegKey>,
    /// Open handles: handle → (root, path components).
    pub open: BTreeMap<u32, (u32, Vec<String>)>,
    next_handle: u32,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

fn root_name(r: u32) -> &'static str {
    match r {
        HKEY_CLASSES_ROOT => "HKEY_CLASSES_ROOT",
        HKEY_CURRENT_USER => "HKEY_CURRENT_USER",
        HKEY_LOCAL_MACHINE => "HKEY_LOCAL_MACHINE",
        HKEY_USERS => "HKEY_USERS",
        HKEY_CURRENT_CONFIG => "HKEY_CURRENT_CONFIG",
        _ => "?",
    }
}

fn parse_root(s: &str) -> Option<u32> {
    Some(match s.to_uppercase().as_str() {
        "HKEY_CLASSES_ROOT" | "HKCR" => HKEY_CLASSES_ROOT,
        "HKEY_CURRENT_USER" | "HKCU" => HKEY_CURRENT_USER,
        "HKEY_LOCAL_MACHINE" | "HKLM" => HKEY_LOCAL_MACHINE,
        "HKEY_USERS" | "HKU" => HKEY_USERS,
        "HKEY_CURRENT_CONFIG" | "HKCC" => HKEY_CURRENT_CONFIG,
        _ => return None,
    })
}

impl Registry {
    pub fn new() -> Self {
        let mut roots = BTreeMap::new();
        for r in [
            HKEY_CLASSES_ROOT,
            HKEY_CURRENT_USER,
            HKEY_LOCAL_MACHINE,
            HKEY_USERS,
            HKEY_CURRENT_CONFIG,
        ] {
            roots.insert(
                r,
                RegKey {
                    name: root_name(r).into(),
                    ..Default::default()
                },
            );
        }
        Registry {
            roots,
            open: BTreeMap::new(),
            next_handle: 0x100,
        }
    }

    /// Resolve an hkey (predefined or open handle) plus a subkey path.
    pub fn resolve(&self, hkey: u32, sub: &str) -> Option<(u32, Vec<String>)> {
        let (root, mut path) = if self.roots.contains_key(&hkey) {
            (hkey, Vec::new())
        } else {
            self.open.get(&hkey)?.clone()
        };
        for c in sub.split('\\').filter(|c| !c.is_empty()) {
            path.push(c.to_string());
        }
        Some((root, path))
    }

    pub fn key(&self, root: u32, path: &[String]) -> Option<&RegKey> {
        let mut k = self.roots.get(&root)?;
        for c in path {
            k = k.subkeys.get(&c.to_uppercase())?;
        }
        Some(k)
    }

    pub fn key_mut(&mut self, root: u32, path: &[String], create: bool) -> Option<&mut RegKey> {
        let mut k = self.roots.get_mut(&root)?;
        for c in path {
            let up = c.to_uppercase();
            if create && !k.subkeys.contains_key(&up) {
                k.subkeys.insert(
                    up.clone(),
                    RegKey {
                        name: c.clone(),
                        ..Default::default()
                    },
                );
            }
            k = k.subkeys.get_mut(&up)?;
        }
        Some(k)
    }

    pub fn open_handle(&mut self, root: u32, path: Vec<String>) -> u32 {
        let h = self.next_handle;
        self.next_handle += 4;
        self.open.insert(h, (root, path));
        h
    }

    pub fn close(&mut self, h: u32) -> bool {
        self.roots.contains_key(&h) || self.open.remove(&h).is_some()
    }

    pub fn set(&mut self, root: u32, path: &[String], name: &str, ty: u32, data: Vec<u8>) {
        if let Some(k) = self.key_mut(root, path, true) {
            k.values.insert(
                name.to_uppercase(),
                RegValue {
                    name: name.to_string(),
                    ty,
                    data,
                },
            );
        }
    }

    /// Import a REGEDIT4 / Windows Registry Editor 5.00 file (ANSI or UTF-16LE).
    pub fn import_reg(&mut self, bytes: &[u8]) -> Result<usize, String> {
        let text = if bytes.starts_with(&[0xff, 0xfe]) {
            let u: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&u)
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        // Join continuation lines ending in backslash.
        let mut lines = Vec::new();
        let mut cur = String::new();
        for l in text.lines() {
            let l = l.trim_end_matches('\r');
            if let Some(stripped) = l.strip_suffix('\\') {
                cur.push_str(stripped.trim_start());
                continue;
            }
            cur.push_str(l.trim_start());
            lines.push(std::mem::take(&mut cur));
        }
        let mut cur_key: Option<(u32, Vec<String>)> = None;
        let mut count = 0;
        for l in lines {
            let l = l.trim();
            if l.is_empty() || l.starts_with(';') || l.starts_with("REGEDIT") || l.starts_with("Windows Registry") {
                continue;
            }
            if l.starts_with('[') && l.ends_with(']') {
                let inner = &l[1..l.len() - 1];
                let (delete, inner) = match inner.strip_prefix('-') {
                    Some(s) => (true, s),
                    None => (false, inner),
                };
                let mut parts = inner.split('\\');
                let root =
                    parse_root(parts.next().unwrap_or("")).ok_or_else(|| format!("unknown root in [{inner}]"))?;
                let path: Vec<String> = parts.map(|s| s.to_string()).collect();
                if delete {
                    if let Some((last, parent)) = path.split_last()
                        && let Some(k) = self.key_mut(root, parent, false)
                    {
                        k.subkeys.remove(&last.to_uppercase());
                    }
                    cur_key = None;
                } else {
                    self.key_mut(root, &path, true);
                    cur_key = Some((root, path));
                }
                continue;
            }
            let Some((root, path)) = &cur_key else {
                continue;
            };
            let (name, rest) = if let Some(r) = l.strip_prefix('@') {
                (String::new(), r.trim_start())
            } else if l.starts_with('"') {
                let end = find_quote_end(l, 1).ok_or("unterminated value name")?;
                (unescape(&l[1..end]), l[end + 1..].trim_start())
            } else {
                continue;
            };
            let rest = rest.strip_prefix('=').ok_or("missing '='")?.trim();
            let (ty, data) = if rest.starts_with('"') {
                let end = find_quote_end(rest, 1).ok_or("unterminated string")?;
                let mut d = unescape(&rest[1..end]).into_bytes();
                d.push(0);
                (REG_SZ, d)
            } else if let Some(h) = rest.strip_prefix("dword:") {
                let v = u32::from_str_radix(h.trim(), 16).map_err(|e| e.to_string())?;
                (REG_DWORD, v.to_le_bytes().to_vec())
            } else if let Some(h) = rest.strip_prefix("hex") {
                let (ty, h) = if let Some(r) = h.strip_prefix('(') {
                    let close = r.find(')').ok_or("bad hex type")?;
                    (
                        u32::from_str_radix(&r[..close], 16).map_err(|e| e.to_string())?,
                        &r[close + 1..],
                    )
                } else {
                    (REG_BINARY, h)
                };
                let h = h.strip_prefix(':').ok_or("bad hex")?;
                let d = h
                    .split(',')
                    .map(|b| b.trim())
                    .filter(|b| !b.is_empty())
                    .map(|b| u8::from_str_radix(b, 16).map_err(|e| e.to_string()))
                    .collect::<Result<Vec<u8>, _>>()?;
                (ty, d)
            } else if rest == "-" {
                if let Some(k) = self.key_mut(*root, path, false) {
                    k.values.remove(&name.to_uppercase());
                }
                continue;
            } else {
                return Err(format!("unsupported value: {rest}"));
            };
            let (root, path) = (*root, path.clone());
            self.set(root, &path, &name, ty, data);
            count += 1;
        }
        Ok(count)
    }

    /// Export the whole registry in REGEDIT4 format.
    pub fn export_reg(&self) -> String {
        let mut out = String::from("REGEDIT4\r\n");
        fn walk(out: &mut String, prefix: &str, k: &RegKey) {
            if !k.values.is_empty() || k.subkeys.is_empty() {
                out.push_str(&format!("\r\n[{prefix}]\r\n"));
                for v in k.values.values() {
                    let name = if v.name.is_empty() {
                        "@".to_string()
                    } else {
                        format!("\"{}\"", escape(&v.name))
                    };
                    let val = match v.ty {
                        REG_SZ => {
                            let s = v.data.strip_suffix(&[0]).unwrap_or(&v.data);
                            // Strings with embedded NULs or non-ASCII bytes are written as
                            // raw bytes so that they read back exactly.
                            if s.iter().all(|&c| (0x20..0x7f).contains(&c) || c == b'\t') {
                                format!("\"{}\"", escape(&String::from_utf8_lossy(s)))
                            } else {
                                format!("hex(1):{}", hexlist(&v.data))
                            }
                        }
                        REG_DWORD if v.data.len() == 4 => {
                            format!("dword:{:08x}", u32::from_le_bytes(v.data[..4].try_into().unwrap()))
                        }
                        REG_BINARY => format!("hex:{}", hexlist(&v.data)),
                        t => format!("hex({t:x}):{}", hexlist(&v.data)),
                    };
                    out.push_str(&format!("{name}={val}\r\n"));
                }
            }
            for s in k.subkeys.values() {
                walk(out, &format!("{prefix}\\{}", s.name), s);
            }
        }
        for (r, k) in &self.roots {
            for s in k.subkeys.values() {
                walk(&mut out, &format!("{}\\{}", root_name(*r), s.name), s);
            }
        }
        out
    }
}

fn hexlist(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(",")
}

fn find_quote_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_export_roundtrip() {
        let mut r = Registry::new();
        let n = r
            .import_reg(
                b"REGEDIT4\r\n\r\n[HKEY_LOCAL_MACHINE\\Software\\Vendor\\App]\r\n\"Path\"=\"C:\\\\App\"\r\n\"Count\"=dword:0000002a\r\n@=\"default\"\r\n\"Bin\"=hex:01,02,\\\r\n  03\r\n",
            )
            .unwrap();
        assert_eq!(n, 4);
        let (root, path) = r.resolve(HKEY_LOCAL_MACHINE, "SOFTWARE\\vendor\\APP").unwrap();
        let k = r.key(root, &path).unwrap();
        assert_eq!(k.values["PATH"].data, b"C:\\App\0");
        assert_eq!(k.values["COUNT"].data, 42u32.to_le_bytes());
        assert_eq!(k.values["BIN"].data, vec![1, 2, 3]);
        let text = r.export_reg();
        let mut r2 = Registry::new();
        r2.import_reg(text.as_bytes()).unwrap();
        assert_eq!(r2.key(root, &path).unwrap().values["PATH"].data, b"C:\\App\0");
    }
}
