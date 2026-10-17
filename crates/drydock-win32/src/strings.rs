//! Code-page conversions (Windows-1252 ANSI) and guest string helpers.

use crate::api::ApiError;
use crate::session::Session;

const CP1252_80: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D,
    0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
    0x0153, 0x009D, 0x017E, 0x0178,
];

pub fn ansi_to_utf16(b: &[u8]) -> Vec<u16> {
    b.iter()
        .map(|&c| match c {
            0x80..=0x9f => CP1252_80[(c - 0x80) as usize],
            _ => c as u16,
        })
        .collect()
}

pub fn decode_ansi(b: &[u8]) -> String {
    String::from_utf16_lossy(&ansi_to_utf16(b))
}

pub fn utf16_to_ansi(w: &[u16]) -> Vec<u8> {
    w.iter()
        .map(|&c| {
            if c < 0x80 || (0xa0..=0xff).contains(&c) {
                c as u8
            } else if let Some(i) = CP1252_80.iter().position(|&x| x == c) {
                0x80 + i as u8
            } else {
                b'?'
            }
        })
        .collect()
}

pub fn encode_ansi(s: &str) -> Vec<u8> {
    let w: Vec<u16> = s.encode_utf16().collect();
    utf16_to_ansi(&w)
}

impl Session {
    /// Read a NUL-terminated ANSI string (bounded to 64 KiB).
    pub fn read_str(&self, p: u32) -> Result<String, ApiError> {
        Ok(decode_ansi(&self.mem.read_cstr(p, 0x10000)?))
    }
    pub fn read_str_opt(&self, p: u32) -> Result<Option<String>, ApiError> {
        if p == 0 { Ok(None) } else { self.read_str(p).map(Some) }
    }
    pub fn read_wstr(&self, p: u32) -> Result<String, ApiError> {
        Ok(String::from_utf16_lossy(&self.mem.read_wstr(p, 0x10000)?))
    }
    pub fn read_wstr_opt(&self, p: u32) -> Result<Option<String>, ApiError> {
        if p == 0 { Ok(None) } else { self.read_wstr(p).map(Some) }
    }
    /// Read a string argument that may be an integer atom/resource id (< 0x10000).
    pub fn read_str_or_id(&self, p: u32) -> Result<StrOrId, ApiError> {
        if p < 0x10000 {
            Ok(StrOrId::Id(p))
        } else {
            Ok(StrOrId::Str(self.read_str(p)?))
        }
    }

    /// Write `s` + NUL into a buffer of `cap` bytes. Returns the number of bytes written
    /// excluding the NUL, or `Err(needed)` (including NUL) if it does not fit.
    pub fn write_str_buf(&mut self, p: u32, cap: u32, s: &str) -> Result<Result<u32, u32>, ApiError> {
        let b = encode_ansi(s);
        if b.len() as u32 + 1 > cap {
            return Ok(Err(b.len() as u32 + 1));
        }
        self.mem.write(p, &b)?;
        self.mem.write_u8(p + b.len() as u32, 0)?;
        Ok(Ok(b.len() as u32))
    }

    /// Write a string truncated to fit (always NUL-terminated if cap > 0).
    pub fn write_str_trunc(&mut self, p: u32, cap: u32, s: &str) -> Result<u32, ApiError> {
        if cap == 0 {
            return Ok(0);
        }
        let mut b = encode_ansi(s);
        b.truncate(cap as usize - 1);
        self.mem.write(p, &b)?;
        self.mem.write_u8(p + b.len() as u32, 0)?;
        Ok(b.len() as u32)
    }

    pub fn write_wstr_buf(&mut self, p: u32, cap_chars: u32, s: &str) -> Result<Result<u32, u32>, ApiError> {
        let w: Vec<u16> = s.encode_utf16().collect();
        if w.len() as u32 + 1 > cap_chars {
            return Ok(Err(w.len() as u32 + 1));
        }
        let mut bytes: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
        bytes.extend([0, 0]);
        self.mem.write(p, &bytes)?;
        Ok(Ok(w.len() as u32))
    }

    /// Allocate a NUL-terminated ANSI copy of `s` in runtime-owned memory.
    pub fn alloc_str(&mut self, s: &str) -> Result<u32, ApiError> {
        let mut b = encode_ansi(s);
        b.push(0);
        let p = self.sys_alloc(b.len() as u32)?;
        self.mem.write(p, &b)?;
        Ok(p)
    }

    pub fn alloc_wstr(&mut self, s: &str) -> Result<u32, ApiError> {
        let mut bytes: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        bytes.extend([0, 0]);
        let p = self.sys_alloc(bytes.len() as u32)?;
        self.mem.write(p, &bytes)?;
        Ok(p)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrOrId {
    Str(String),
    Id(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cp1252_roundtrip() {
        let b: Vec<u8> = (1..=255u8).collect();
        let s = decode_ansi(&b);
        assert!(s.contains('€'));
        assert_eq!(encode_ansi(&s), b);
    }
}
