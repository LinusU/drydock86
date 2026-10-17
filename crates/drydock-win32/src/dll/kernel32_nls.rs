//! kernel32: code pages, character types, string comparison, locale information.
//!
//! The emulated locale is en-US with ANSI code page 1252. Linguistic comparison uses a
//! documented simplification: case-insensitive primary ordering by code point with
//! lower case sorting before upper case for otherwise equal strings.

use super::kernel32::*;
use crate::api::*;
use crate::session::Session;
use crate::strings::{ansi_to_utf16, utf16_to_ansi};

pub fn get_acp(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.config.windows.ansi_codepage))
}
pub fn get_oemcp(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.config.windows.oem_codepage))
}
pub fn get_system_default_lang_id(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.config.windows.lcid & 0xffff))
}
pub fn get_user_default_lcid(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.config.windows.lcid))
}
pub fn is_dbcs_lead_byte(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}
pub fn is_valid_code_page(s: &mut Session) -> ApiResult {
    let [cp] = s.args::<1>()?;
    Ok(ApiRet::Ret(
        matches!(cp, 1252 | 437 | 850 | 20127 | 28591 | 65001) as u32
    ))
}

pub fn get_cp_info(s: &mut Session) -> ApiResult {
    let [cp, p] = s.args::<2>()?;
    if !matches!(cp, 0 | 1 | 1252 | 437 | 850 | 3) {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    let mut b = vec![0u8; 20];
    b[0..4].copy_from_slice(&1u32.to_le_bytes());
    b[4] = b'?';
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(1))
}

fn check_cp(s: &mut Session, cp: u32) -> bool {
    if matches!(cp, 0 | 1 | 2 | 3 | 1252 | 437 | 850 | 20127 | 28591) {
        true
    } else {
        s.missing("codepage", format!("code page {cp} is not implemented"));
        false
    }
}

pub fn multi_byte_to_wide_char(s: &mut Session) -> ApiResult {
    let [cp, _flags, src, n, dst, cap] = s.args::<6>()?;
    if !check_cp(s, cp) {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    let bytes = if n == 0xffff_ffff {
        let mut b = s.mem.read_cstr(src, 0x100000)?;
        b.push(0);
        b
    } else {
        s.mem.read_vec(src, n)?
    };
    let w = ansi_to_utf16(&bytes);
    if cap == 0 {
        return Ok(ApiRet::Ret(w.len() as u32));
    }
    if w.len() as u32 > cap {
        s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(ApiRet::Ret(0));
    }
    let out: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    s.mem.write(dst, &out)?;
    Ok(ApiRet::Ret(w.len() as u32))
}

pub fn wide_char_to_multi_byte(s: &mut Session) -> ApiResult {
    let [cp, _flags, src, n, dst, cap, _defchar, used] = s.args::<8>()?;
    if !check_cp(s, cp) {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    let w: Vec<u16> = if n == 0xffff_ffff {
        let mut v = s.mem.read_wstr(src, 0x100000)?;
        v.push(0);
        v
    } else {
        let raw = s.mem.read_vec(src, n * 2)?;
        raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    };
    let b = utf16_to_ansi(&w);
    if used != 0 {
        let any = w.iter().zip(&b).any(|(&c, &x)| x == b'?' && c != b'?' as u16);
        s.mem.write_u32(used, any as u32)?;
    }
    if cap == 0 {
        return Ok(ApiRet::Ret(b.len() as u32));
    }
    if b.len() as u32 > cap {
        s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(ApiRet::Ret(0));
    }
    s.mem.write(dst, &b)?;
    Ok(ApiRet::Ret(b.len() as u32))
}

// C1 character type bits.
const C1_UPPER: u16 = 0x1;
const C1_LOWER: u16 = 0x2;
const C1_DIGIT: u16 = 0x4;
const C1_SPACE: u16 = 0x8;
const C1_PUNCT: u16 = 0x10;
const C1_CNTRL: u16 = 0x20;
const C1_BLANK: u16 = 0x40;
const C1_XDIGIT: u16 = 0x80;
const C1_ALPHA: u16 = 0x100;

pub fn ctype1(c: u16) -> u16 {
    let ch = char::from_u32(c as u32).unwrap_or('\0');
    let mut t = 0;
    if ch.is_uppercase() {
        t |= C1_UPPER | C1_ALPHA;
    }
    if ch.is_lowercase() {
        t |= C1_LOWER | C1_ALPHA;
    }
    if ch.is_alphabetic() {
        t |= C1_ALPHA;
    }
    if ch.is_ascii_digit() {
        t |= C1_DIGIT;
    }
    if ch.is_ascii_hexdigit() {
        t |= C1_XDIGIT;
    }
    if matches!(c, 0x09..=0x0d | 0x20 | 0xa0) {
        t |= C1_SPACE;
    }
    if c == 0x20 || c == 0x09 || c == 0xa0 {
        t |= C1_BLANK;
    }
    if ch.is_control() {
        t |= C1_CNTRL;
    }
    if (ch.is_ascii_punctuation() || (!ch.is_alphanumeric() && !ch.is_whitespace() && !ch.is_control())) && c != 0 {
        t |= C1_PUNCT;
    }
    t
}

fn string_type(s: &mut Session, info: u32, w: &[u16], out: u32) -> ApiResult {
    if info != 1 {
        s.missing("GetStringType", format!("info type {info}"));
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    for (i, &c) in w.iter().enumerate() {
        s.mem.write_u16(out + 2 * i as u32, ctype1(c))?;
    }
    Ok(ApiRet::Ret(1))
}

fn read_counted_a(s: &Session, p: u32, n: u32) -> Result<Vec<u8>, ApiError> {
    if n == 0xffff_ffff {
        Ok(s.mem.read_cstr(p, 0x100000)?)
    } else {
        Ok(s.mem.read_vec(p, n)?)
    }
}
fn read_counted_w(s: &Session, p: u32, n: u32) -> Result<Vec<u16>, ApiError> {
    if n == 0xffff_ffff {
        Ok(s.mem.read_wstr(p, 0x100000)?)
    } else {
        let raw = s.mem.read_vec(p, n * 2)?;
        Ok(raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }
}

pub fn get_string_type_a(s: &mut Session) -> ApiResult {
    let [_lcid, info, src, n, out] = s.args::<5>()?;
    let b = read_counted_a(s, src, n)?;
    let w = ansi_to_utf16(&b);
    string_type(s, info, &w, out)
}
pub fn get_string_type_ex_a(s: &mut Session) -> ApiResult {
    get_string_type_a(s)
}
pub fn get_string_type_w(s: &mut Session) -> ApiResult {
    let [info, src, n, out] = s.args::<4>()?;
    let w = read_counted_w(s, src, n)?;
    string_type(s, info, &w, out)
}

const LCMAP_LOWERCASE: u32 = 0x100;
const LCMAP_UPPERCASE: u32 = 0x200;

fn map_case(s: &mut Session, flags: u32, w: &[u16]) -> Option<Vec<u16>> {
    let st = String::from_utf16_lossy(w);
    let r = if flags & LCMAP_UPPERCASE != 0 {
        st.chars()
            .map(|c| c.to_uppercase().next().unwrap_or(c))
            .collect::<String>()
    } else if flags & LCMAP_LOWERCASE != 0 {
        st.chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect::<String>()
    } else {
        s.missing("LCMapString", format!("flags {flags:#x}"));
        return None;
    };
    Some(r.encode_utf16().collect())
}

pub fn lc_map_string_a(s: &mut Session) -> ApiResult {
    let [_lcid, flags, src, n, dst, cap] = s.args::<6>()?;
    let mut b = read_counted_a(s, src, n)?;
    if n == 0xffff_ffff {
        b.push(0);
    }
    let Some(w) = map_case(s, flags, &ansi_to_utf16(&b)) else {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    };
    let out = utf16_to_ansi(&w);
    if cap == 0 {
        return Ok(ApiRet::Ret(out.len() as u32));
    }
    if out.len() as u32 > cap {
        s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(ApiRet::Ret(0));
    }
    s.mem.write(dst, &out)?;
    Ok(ApiRet::Ret(out.len() as u32))
}

pub fn lc_map_string_w(s: &mut Session) -> ApiResult {
    let [_lcid, flags, src, n, dst, cap] = s.args::<6>()?;
    let mut w = read_counted_w(s, src, n)?;
    if n == 0xffff_ffff {
        w.push(0);
    }
    let Some(w) = map_case(s, flags, &w) else {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    };
    if cap == 0 {
        return Ok(ApiRet::Ret(w.len() as u32));
    }
    if w.len() as u32 > cap {
        s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(ApiRet::Ret(0));
    }
    let out: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    s.mem.write(dst, &out)?;
    Ok(ApiRet::Ret(w.len() as u32))
}

/// Simplified linguistic comparison (see module docs). Returns -1/0/1.
pub fn linguistic_cmp(a: &[u16], b: &[u16], ignore_case: bool) -> i32 {
    let fold = |c: u16| {
        char::from_u32(c as u32)
            .map(|ch| ch.to_lowercase().next().unwrap_or(ch) as u32)
            .unwrap_or(c as u32)
    };
    for (x, y) in a.iter().zip(b) {
        let (fx, fy) = (fold(*x), fold(*y));
        if fx != fy {
            return if fx < fy { -1 } else { 1 };
        }
    }
    if a.len() != b.len() {
        return if a.len() < b.len() { -1 } else { 1 };
    }
    if !ignore_case {
        for (x, y) in a.iter().zip(b) {
            if x != y {
                // Lower case sorts before upper case.
                let xl = ctype1(*x) & C1_LOWER != 0;
                return if xl { -1 } else { 1 };
            }
        }
    }
    0
}

fn compare_result(c: i32) -> u32 {
    (c + 2) as u32
}

pub fn compare_string_a(s: &mut Session) -> ApiResult {
    let [_lcid, flags, a, na, b, nb] = s.args::<6>()?;
    let a = ansi_to_utf16(&read_counted_a(s, a, na)?);
    let b = ansi_to_utf16(&read_counted_a(s, b, nb)?);
    Ok(ApiRet::Ret(compare_result(linguistic_cmp(&a, &b, flags & 1 != 0))))
}
pub fn compare_string_w(s: &mut Session) -> ApiResult {
    let [_lcid, flags, a, na, b, nb] = s.args::<6>()?;
    let a = read_counted_w(s, a, na)?;
    let b = read_counted_w(s, b, nb)?;
    Ok(ApiRet::Ret(compare_result(linguistic_cmp(&a, &b, flags & 1 != 0))))
}

pub fn lstrcmp_a(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    let a = ansi_to_utf16(&s.mem.read_cstr(a, 0x100000)?);
    let b = ansi_to_utf16(&s.mem.read_cstr(b, 0x100000)?);
    Ok(ApiRet::Ret(linguistic_cmp(&a, &b, false) as u32))
}
pub fn lstrcmpi_a(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    let a = ansi_to_utf16(&s.mem.read_cstr(a, 0x100000)?);
    let b = ansi_to_utf16(&s.mem.read_cstr(b, 0x100000)?);
    Ok(ApiRet::Ret(linguistic_cmp(&a, &b, true) as u32))
}
pub fn lstrlen_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p == 0 {
        return Ok(ApiRet::Ret(0));
    }
    // lstrlen guards against bad pointers and returns 0.
    Ok(ApiRet::Ret(
        s.mem.read_cstr(p, 0x1000_0000).map(|v| v.len() as u32).unwrap_or(0),
    ))
}
pub fn lstrlen_w(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p == 0 {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(
        s.mem.read_wstr(p, 0x1000_0000).map(|v| v.len() as u32).unwrap_or(0),
    ))
}
pub fn lstrcpy_a(s: &mut Session) -> ApiResult {
    let [d, src] = s.args::<2>()?;
    let mut b = s.mem.read_cstr(src, 0x1000_0000)?;
    b.push(0);
    s.mem.write(d, &b)?;
    Ok(ApiRet::Ret(d))
}
pub fn lstrcpyn_a(s: &mut Session) -> ApiResult {
    let [d, src, n] = s.args::<3>()?;
    if n == 0 {
        return Ok(ApiRet::Ret(d));
    }
    let mut b = s.mem.read_cstr(src, n - 1)?;
    b.truncate(n as usize - 1);
    b.push(0);
    s.mem.write(d, &b)?;
    Ok(ApiRet::Ret(d))
}
pub fn lstrcat_a(s: &mut Session) -> ApiResult {
    let [d, src] = s.args::<2>()?;
    let len = s.mem.read_cstr(d, 0x1000_0000)?.len() as u32;
    let mut b = s.mem.read_cstr(src, 0x1000_0000)?;
    b.push(0);
    s.mem.write(d + len, &b)?;
    Ok(ApiRet::Ret(d))
}

pub fn get_locale_info_a(s: &mut Session) -> ApiResult {
    let [_lcid, lctype, buf, n] = s.args::<4>()?;
    let v: String = match lctype & 0xffff {
        0x1 => "0409".into(),                    // LOCALE_ILANGUAGE
        0x2 => "English (United States)".into(), // LOCALE_SLANGUAGE
        0x3 => "ENU".into(),                     // LOCALE_SABBREVLANGNAME
        0x7 => "United States".into(),           // LOCALE_SCOUNTRY (SENGCOUNTRY=0x1002)
        0x5 => "1".into(),                       // LOCALE_ICOUNTRY
        0xb => "437".into(),                     // LOCALE_IDEFAULTCODEPAGE
        0x1004 => "1252".into(),                 // LOCALE_IDEFAULTANSICODEPAGE
        0xc => ",".into(),                       // LOCALE_SLIST
        0xe => ".".into(),                       // LOCALE_SDECIMAL
        0xf => ",".into(),                       // LOCALE_STHOUSAND
        0x10 => "3;0".into(),                    // LOCALE_SGROUPING
        0x11 => "2".into(),                      // LOCALE_IDIGITS
        0x14 => "$".into(),                      // LOCALE_SCURRENCY
        0x1d => "/".into(),                      // LOCALE_SDATE
        0x1e => ":".into(),                      // LOCALE_STIME
        0x1f => "M/d/yyyy".into(),               // LOCALE_SSHORTDATE
        0x20 => "dddd, MMMM dd, yyyy".into(),    // LOCALE_SLONGDATE
        0x1001 => "English".into(),              // LOCALE_SENGLANGUAGE
        0x1002 => "United States".into(),        // LOCALE_SENGCOUNTRY
        0x59 => "en".into(),                     // LOCALE_SISO639LANGNAME
        0x5a => "US".into(),                     // LOCALE_SISO3166CTRYNAME
        t => {
            s.missing("GetLocaleInfo", format!("LCTYPE {t:#x}"));
            s.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(ApiRet::Ret(0));
        }
    };
    if n == 0 {
        return Ok(ApiRet::Ret(v.len() as u32 + 1));
    }
    match s.write_str_buf(buf, n, &v)? {
        Ok(len) => Ok(ApiRet::Ret(len + 1)),
        Err(_) => {
            s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
            Ok(ApiRet::Ret(0))
        }
    }
}
