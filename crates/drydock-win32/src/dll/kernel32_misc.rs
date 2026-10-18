//! kernel32: profile (.ini) files, message and date/time formatting, process queries.

use super::kernel32::{ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, filetime_to_systemtime};
use crate::api::*;
use crate::export;
use crate::session::Session;
use crate::strings::{decode_ansi, encode_ansi};
use crate::vfs::{Disposition, GuestPath};

fn get_process_version(s: &mut Session) -> ApiResult {
    let [pid] = s.args::<1>()?;
    if pid != 0 && pid != s.process.pid {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    // MajorSubsystemVersion/MinorSubsystemVersion of the executable's optional header.
    let base = s.process.image_base;
    let nt = base + s.mem.read_u32(base + 0x3c)?;
    let major = s.mem.read_u16(nt + 0x18 + 48)? as u32;
    let minor = s.mem.read_u16(nt + 0x18 + 50)? as u32;
    Ok(ApiRet::Ret((major << 16) | minor))
}

fn get_process_affinity_mask(s: &mut Session) -> ApiResult {
    let [_h, proc_mask, sys_mask] = s.args::<3>()?;
    s.mem.write_u32(proc_mask, 1)?;
    s.mem.write_u32(sys_mask, 1)?;
    Ok(ApiRet::Ret(1))
}

fn beep(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(1))
}

fn is_valid_locale(s: &mut Session) -> ApiResult {
    let [lcid, _flags] = s.args::<2>()?;
    Ok(ApiRet::Ret(matches!(lcid & 0xffff, 0x409 | 0x400 | 0x800 | 0x7f) as u32))
}

fn find_resource_ex_a(s: &mut Session) -> ApiResult {
    // The language is ignored: modules carry one language per resource.
    let [h, ty, name, _lang] = s.args::<4>()?;
    let esp = s.cpu.esp();
    // Reuse FindResourceA(hModule, lpName, lpType) by rewriting the argument order in place.
    s.mem.write_u32(esp + 4, h)?;
    s.mem.write_u32(esp + 8, name)?;
    s.mem.write_u32(esp + 12, ty)?;
    super::kernel32::find_resource_a(s)
}

// ---------------------------------------------------------------- files

fn get_file_information_by_handle(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let Some((path, size)) = super::kernel32_file::with_file(s, h, |s, f| (f.path.clone(), s.vfs.file_size(f))) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(0));
    };
    let e = s.vfs.stat(&path).ok();
    let attrs = e.as_ref().map(|e| e.attrs).unwrap_or(0x20);
    let mtime = e.as_ref().map(|e| e.mtime).unwrap_or(0);
    let serial = s.vfs.drives.get(&path.drive).map(|d| d.serial).unwrap_or(0);
    // A stable file index from the path.
    let mut idx: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.key().bytes() {
        idx = (idx ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    let mut b = vec![0u8; 52];
    b[0..4].copy_from_slice(&attrs.to_le_bytes());
    for o in [4, 12, 20] {
        b[o..o + 8].copy_from_slice(&mtime.to_le_bytes());
    }
    b[28..32].copy_from_slice(&serial.to_le_bytes());
    b[32..36].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
    b[36..40].copy_from_slice(&(size as u32).to_le_bytes());
    b[40..44].copy_from_slice(&1u32.to_le_bytes());
    b[44..48].copy_from_slice(&((idx >> 32) as u32).to_le_bytes());
    b[48..52].copy_from_slice(&(idx as u32).to_le_bytes());
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(1))
}

/// LockFile/UnlockFile: only this process accesses the files, so locks never conflict.
fn lock_file(s: &mut Session) -> ApiResult {
    let [h, ..] = s.args::<5>()?;
    let ok = super::kernel32_file::with_file(s, h, |_, _| ()).is_some();
    if !ok {
        s.set_last_error(ERROR_INVALID_HANDLE);
    }
    Ok(ApiRet::Ret(ok as u32))
}

fn set_file_time(s: &mut Session) -> ApiResult {
    let [h, ..] = s.args::<4>()?;
    // Timestamps of guest files are not stored separately from their contents.
    let ok = super::kernel32_file::with_file(s, h, |_, _| ()).is_some();
    if !ok {
        s.set_last_error(ERROR_INVALID_HANDLE);
    }
    Ok(ApiRet::Ret(ok as u32))
}

fn get_short_path_name_a(s: &mut Session) -> ApiResult {
    let [p, buf, n] = s.args::<3>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(123);
        return Ok(ApiRet::Ret(0));
    };
    if s.vfs.attributes(&gp).is_err() {
        s.set_last_error(2);
        return Ok(ApiRet::Ret(0));
    }
    // Names are returned unchanged (correct for 8.3 names, which installations use).
    match s.write_str_buf(buf, n, &name)? {
        Ok(len) => Ok(ApiRet::Ret(len)),
        Err(need) => Ok(ApiRet::Ret(need)),
    }
}

// ---------------------------------------------------------------- profile files

fn read_guest_file(s: &mut Session, p: &GuestPath) -> Option<Vec<u8>> {
    let now = s.filetime_now();
    let (mut f, _) = s.vfs.open(p, true, false, Disposition::OpenExisting, now).ok()?;
    let mut out = vec![0u8; s.vfs.file_size(&f) as usize];
    let n = s.vfs.read(&mut f, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

fn write_guest_file(s: &mut Session, p: &GuestPath, data: &[u8]) -> bool {
    let now = s.filetime_now();
    let Ok((mut f, _)) = s.vfs.open(p, true, true, Disposition::CreateAlways, now) else {
        return false;
    };
    s.vfs.write(&mut f, data, now).is_ok()
}

/// A profile name without a path refers to the Windows directory.
fn profile_path(s: &Session, file: Option<&str>) -> Option<GuestPath> {
    let f = file.unwrap_or("win.ini");
    if f.contains('\\') || f.contains('/') || f.contains(':') {
        s.guest_path(f)
    } else {
        s.guest_path(&format!("{}\\{f}", s.config.windows.windows_dir))
    }
}

struct Ini {
    /// (section, [(key, value)]) in file order; raw lines are kept for rewriting.
    lines: Vec<String>,
}

impl Ini {
    fn parse(data: &[u8]) -> Ini {
        let text = decode_ansi(data);
        Ini {
            lines: text.lines().map(|l| l.to_string()).collect(),
        }
    }
    fn section_name(line: &str) -> Option<&str> {
        let t = line.trim();
        t.strip_prefix('[').and_then(|r| r.find(']').map(|i| r[..i].trim()))
    }
    fn key_value(line: &str) -> Option<(&str, &str)> {
        let t = line.trim_start();
        if t.starts_with(';') {
            return None;
        }
        let i = t.find('=')?;
        Some((t[..i].trim(), t[i + 1..].trim()))
    }
    fn sections(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|l| Self::section_name(l).map(String::from))
            .collect()
    }
    /// Line range of a section's body.
    fn section_range(&self, sec: &str) -> Option<(usize, usize)> {
        let start = self
            .lines
            .iter()
            .position(|l| Self::section_name(l).is_some_and(|n| n.eq_ignore_ascii_case(sec)))?;
        let end = self.lines[start + 1..]
            .iter()
            .position(|l| Self::section_name(l).is_some())
            .map(|i| start + 1 + i)
            .unwrap_or(self.lines.len());
        Some((start + 1, end))
    }
    fn keys(&self, sec: &str) -> Vec<String> {
        let Some((a, b)) = self.section_range(sec) else {
            return vec![];
        };
        self.lines[a..b]
            .iter()
            .filter_map(|l| Self::key_value(l).map(|(k, _)| k.to_string()))
            .collect()
    }
    fn get(&self, sec: &str, key: &str) -> Option<String> {
        let (a, b) = self.section_range(sec)?;
        self.lines[a..b].iter().find_map(|l| {
            Self::key_value(l)
                .filter(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.to_string())
        })
    }
    fn set(&mut self, sec: &str, key: Option<&str>, val: Option<&str>) {
        let range = self.section_range(sec);
        let Some(key) = key else {
            // Delete the whole section.
            if let Some((a, b)) = range {
                self.lines.drain(a - 1..b);
            }
            return;
        };
        let (a, b) = match range {
            Some(r) => r,
            None => {
                if val.is_none() {
                    return;
                }
                self.lines.push(format!("[{sec}]"));
                let n = self.lines.len();
                (n, n)
            }
        };
        let pos = (a..b).find(|&i| Self::key_value(&self.lines[i]).is_some_and(|(k, _)| k.eq_ignore_ascii_case(key)));
        match (pos, val) {
            (Some(i), Some(v)) => self.lines[i] = format!("{key}={v}"),
            (Some(i), None) => {
                self.lines.remove(i);
            }
            (None, Some(v)) => {
                // Insert after the last non-empty line of the section.
                let mut at = b;
                while at > a && self.lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                self.lines.insert(at, format!("{key}={v}"));
            }
            (None, None) => {}
        }
    }
    fn bytes(&self) -> Vec<u8> {
        let mut t = self.lines.join("\r\n");
        t.push_str("\r\n");
        encode_ansi(&t)
    }
}

fn load_ini(s: &mut Session, file: Option<&str>) -> Ini {
    let data = profile_path(s, file)
        .and_then(|p| read_guest_file(s, &p))
        .unwrap_or_default();
    Ini::parse(&data)
}

/// Write a list of NUL-terminated strings followed by a final NUL, truncated to fit;
/// returns the GetPrivateProfileString count (size - 2 when truncated).
fn write_multi(s: &mut Session, buf: u32, n: u32, items: &[String]) -> Result<u32, ApiError> {
    if n == 0 {
        return Ok(0);
    }
    let mut out = Vec::new();
    for it in items {
        out.extend(encode_ansi(it));
        out.push(0);
    }
    if out.len() + 1 > n as usize {
        if n < 2 {
            s.mem.write_u8(buf, 0)?;
            return Ok(0);
        }
        out.truncate(n as usize - 2);
        out.extend([0, 0]);
        s.mem.write(buf, &out)?;
        return Ok(n - 2);
    }
    out.push(0);
    s.mem.write(buf, &out)?;
    Ok(out.len() as u32 - 1)
}

fn profile_string(s: &mut Session, sec: u32, key: u32, def: u32, buf: u32, n: u32, file: Option<String>) -> ApiResult {
    let ini = load_ini(s, file.as_deref());
    let sec = s.read_str_opt(sec)?;
    let key = s.read_str_opt(key)?;
    let Some(sec) = sec else {
        let names = ini.sections();
        return Ok(ApiRet::Ret(write_multi(s, buf, n, &names)?));
    };
    let Some(key) = key else {
        let keys = ini.keys(&sec);
        return Ok(ApiRet::Ret(write_multi(s, buf, n, &keys)?));
    };
    let v = match ini.get(&sec, &key) {
        Some(v) => v,
        None => s.read_str_opt(def)?.unwrap_or_default().trim_end().to_string(),
    };
    // Values in quotes are returned without them.
    let v = if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].to_string()
    } else {
        v
    };
    Ok(ApiRet::Ret(s.write_str_trunc(buf, n, &v)?))
}

fn get_private_profile_string_a(s: &mut Session) -> ApiResult {
    let [sec, key, def, buf, n, file] = s.args::<6>()?;
    let file = s.read_str_opt(file)?;
    profile_string(s, sec, key, def, buf, n, file)
}

fn get_profile_string_a(s: &mut Session) -> ApiResult {
    let [sec, key, def, buf, n] = s.args::<5>()?;
    profile_string(s, sec, key, def, buf, n, None)
}

fn parse_profile_int(v: &str) -> Option<u32> {
    let t = v.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (digits, radix) = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => (h, 16),
        None => (t, 10),
    };
    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    if end == 0 {
        return Some(0);
    }
    let v = u32::from_str_radix(&digits[..end], radix).ok()?;
    Some(if neg { v.wrapping_neg() } else { v })
}

fn profile_int(s: &mut Session, sec: u32, key: u32, def: u32, file: Option<String>) -> ApiResult {
    let ini = load_ini(s, file.as_deref());
    let sec = s.read_str(sec)?;
    let key = s.read_str(key)?;
    Ok(ApiRet::Ret(
        ini.get(&sec, &key).and_then(|v| parse_profile_int(&v)).unwrap_or(def),
    ))
}

fn get_private_profile_int_a(s: &mut Session) -> ApiResult {
    let [sec, key, def, file] = s.args::<4>()?;
    let file = s.read_str_opt(file)?;
    profile_int(s, sec, key, def, file)
}

fn get_profile_int_a(s: &mut Session) -> ApiResult {
    let [sec, key, def] = s.args::<3>()?;
    profile_int(s, sec, key, def, None)
}

fn write_profile(s: &mut Session, sec: u32, key: u32, val: u32, file: Option<String>) -> ApiResult {
    let Some(path) = profile_path(s, file.as_deref()) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    let Some(sec) = s.read_str_opt(sec)? else {
        // Flush request: nothing is cached.
        return Ok(ApiRet::Ret(1));
    };
    let key = s.read_str_opt(key)?;
    let val = s.read_str_opt(val)?;
    let mut ini = load_ini(s, file.as_deref());
    ini.set(&sec, key.as_deref(), val.as_deref());
    if !write_guest_file(s, &path, &ini.bytes()) {
        s.set_last_error(5);
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(1))
}

fn write_private_profile_string_a(s: &mut Session) -> ApiResult {
    let [sec, key, val, file] = s.args::<4>()?;
    let file = s.read_str_opt(file)?;
    write_profile(s, sec, key, val, file)
}

fn write_profile_string_a(s: &mut Session) -> ApiResult {
    let [sec, key, val] = s.args::<3>()?;
    write_profile(s, sec, key, val, None)
}

// ---------------------------------------------------------------- FormatMessage

fn system_message(code: u32) -> Option<&'static str> {
    Some(match code {
        0 => "The operation completed successfully.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough storage is available to process this command.\r\n",
        14 => "Not enough storage is available to complete this operation.\r\n",
        32 => "The process cannot access the file because it is being used by another process.\r\n",
        80 => "The file exists.\r\n",
        87 => "The parameter is incorrect.\r\n",
        112 => "There is not enough space on the disk.\r\n",
        _ => return None,
    })
}

/// Expand %1..%99 inserts (with optional !printf-format!, of which only s, d, u, x
/// and c are supported), %0, %n, %%, and escapes.
fn expand_message(s: &Session, fmt: &str, args: u32, array: bool, ignore: bool) -> Result<String, ApiError> {
    if ignore {
        return Ok(fmt.to_string());
    }
    let mut out = String::new();
    let c: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '%' || i + 1 >= c.len() {
            out.push(c[i]);
            i += 1;
            continue;
        }
        i += 1;
        match c[i] {
            '0' => break,
            'n' => out.push_str("\r\n"),
            't' => out.push('\t'),
            '%' | '.' | '!' | ' ' => out.push(c[i]),
            d if d.is_ascii_digit() => {
                let mut n = d.to_digit(10).unwrap();
                if i + 1 < c.len() && c[i + 1].is_ascii_digit() {
                    i += 1;
                    n = n * 10 + c[i].to_digit(10).unwrap();
                }
                let mut spec = "s".to_string();
                if i + 1 < c.len() && c[i + 1] == '!' {
                    let end = c[i + 2..].iter().position(|&x| x == '!').map(|p| i + 2 + p);
                    if let Some(e) = end {
                        spec = c[i + 2..e].iter().collect();
                        i = e;
                    }
                }
                let v = if args == 0 {
                    0
                } else if array {
                    s.mem.read_u32(args + 4 * (n - 1))?
                } else {
                    // va_list: a pointer to consecutive arguments.
                    let va = s.mem.read_u32(args)?;
                    s.mem.read_u32(va + 4 * (n - 1))?
                };
                match spec.chars().last().unwrap_or('s') {
                    's' => out.push_str(&s.read_str(v)?),
                    'd' | 'i' => out.push_str(&(v as i32).to_string()),
                    'u' => out.push_str(&v.to_string()),
                    'x' => out.push_str(&format!("{v:x}")),
                    'X' => out.push_str(&format!("{v:X}")),
                    'c' => out.push(v as u8 as char),
                    _ => out.push_str(&v.to_string()),
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    Ok(out)
}

fn format_message_a(s: &mut Session) -> ApiResult {
    let [flags, source, id, _lang, buf, n, args] = s.args::<7>()?;
    let alloc = flags & 0x100 != 0;
    let ignore = flags & 0x200 != 0;
    let array = flags & 0x2000 != 0;
    let fmt = if flags & 0x400 != 0 {
        s.read_str(source)?
    } else if flags & 0x1000 != 0 {
        match system_message(id) {
            Some(m) => m.to_string(),
            None => {
                s.set_last_error(317); // ERROR_MR_MID_NOT_FOUND
                return Ok(ApiRet::Ret(0));
            }
        }
    } else {
        s.missing(
            "FormatMessage",
            format!("message tables (flags {flags:#x}, id {id:#x})"),
        );
        s.set_last_error(317);
        return Ok(ApiRet::Ret(0));
    };
    let text = expand_message(s, &fmt, args, array, ignore)?;
    let mut b = encode_ansi(&text);
    let len = b.len() as u32;
    b.push(0);
    if alloc {
        let size = (b.len() as u32).max(n);
        let ph = s.heaps.process_heap;
        let Some(p) = s.heaps.alloc(&mut s.mem, ph, size, true) else {
            s.set_last_error(8);
            return Ok(ApiRet::Ret(0));
        };
        s.mem.write(p, &b)?;
        s.mem.write_u32(buf, p)?;
    } else {
        if b.len() as u32 > n {
            s.set_last_error(122); // ERROR_INSUFFICIENT_BUFFER
            return Ok(ApiRet::Ret(0));
        }
        s.mem.write(buf, &b)?;
    }
    Ok(ApiRet::Ret(len))
}

// ---------------------------------------------------------------- date and time

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn read_time_arg(s: &Session, p: u32) -> Result<[u16; 8], ApiError> {
    if p == 0 {
        return Ok(filetime_to_systemtime(s.local_filetime_now()));
    }
    let mut st = [0u16; 8];
    for (i, v) in st.iter_mut().enumerate() {
        *v = s.mem.read_u16(p + 2 * i as u32)?;
    }
    Ok(st)
}

/// Expand a date or time picture. Quoted text is copied literally.
fn format_picture(pic: &str, st: &[u16; 8], date: bool) -> String {
    let c: Vec<char> = pic.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '\'' {
            i += 1;
            while i < c.len() && c[i] != '\'' {
                out.push(c[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < c.len() && c[i + n] == ch {
            n += 1;
        }
        let (year, month, dow, day, hour, minute, sec) = (st[0], st[1], st[2], st[3], st[4], st[5], st[6]);
        let piece = match (date, ch) {
            (true, 'd') => match n {
                1 => day.to_string(),
                2 => format!("{day:02}"),
                3 => DAYS[dow as usize % 7][..3].to_string(),
                _ => DAYS[dow as usize % 7].to_string(),
            },
            (true, 'M') => match n {
                1 => month.to_string(),
                2 => format!("{month:02}"),
                3 => MONTHS[(month as usize + 11) % 12][..3].to_string(),
                _ => MONTHS[(month as usize + 11) % 12].to_string(),
            },
            (true, 'y') => match n {
                1 => (year % 100).to_string(),
                2 => format!("{:02}", year % 100),
                _ => year.to_string(),
            },
            (true, 'g') => "A.D.".to_string(),
            (false, 'h') => {
                let h12 = if hour % 12 == 0 { 12 } else { hour % 12 };
                if n == 1 { h12.to_string() } else { format!("{h12:02}") }
            }
            (false, 'H') => {
                if n == 1 {
                    hour.to_string()
                } else {
                    format!("{hour:02}")
                }
            }
            (false, 'm') => {
                if n == 1 {
                    minute.to_string()
                } else {
                    format!("{minute:02}")
                }
            }
            (false, 's') => {
                if n == 1 {
                    sec.to_string()
                } else {
                    format!("{sec:02}")
                }
            }
            (false, 't') => {
                let t = if hour < 12 { "AM" } else { "PM" };
                if n == 1 { t[..1].to_string() } else { t.to_string() }
            }
            _ => std::iter::repeat_n(ch, n).collect(),
        };
        out.push_str(&piece);
        i += n;
    }
    out
}

fn date_time_format(s: &mut Session, date: bool) -> ApiResult {
    let [_lcid, flags, st, fmt, buf, n] = s.args::<6>()?;
    let st = read_time_arg(s, st)?;
    let pic = match s.read_str_opt(fmt)? {
        Some(p) => p,
        None if date => if flags & 2 != 0 {
            "dddd, MMMM dd, yyyy"
        } else {
            "M/d/yyyy"
        }
        .to_string(),
        None => {
            if flags & 2 != 0 {
                "h:mm tt".to_string() // TIME_NOSECONDS
            } else {
                "h:mm:ss tt".to_string()
            }
        }
    };
    let text = format_picture(&pic, &st, date);
    if n == 0 {
        return Ok(ApiRet::Ret(encode_ansi(&text).len() as u32 + 1));
    }
    match s.write_str_buf(buf, n, &text)? {
        Ok(len) => Ok(ApiRet::Ret(len + 1)),
        Err(_) => {
            s.set_last_error(122);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn get_date_format_a(s: &mut Session) -> ApiResult {
    date_time_format(s, true)
}

fn get_time_format_a(s: &mut Session) -> ApiResult {
    date_time_format(s, false)
}

/// The emulated system has one locale installed: English (United States).
fn enum_system_locales_a(s: &mut Session) -> ApiResult {
    let [cb, _flags] = s.args::<2>()?;
    if cb == 0 {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    let items = vec![vec![CallArg::Buf(b"00000409\0".to_vec())]];
    crate::com::enum_callbacks(cb, items, 0, 1)
}

pub static KERNEL32_MISC_EXPORTS: &[ExportDef] = &[
    export!("EnumSystemLocalesA", 2, enum_system_locales_a),
    export!("GetLocaleInfoW", 4, super::kernel32_nls::get_locale_info_w),
    export!("Beep", 2, beep),
    export!("FindResourceExA", 4, find_resource_ex_a),
    export!("FormatMessageA", 7, format_message_a),
    export!("GetDateFormatA", 6, get_date_format_a),
    export!("GetFileInformationByHandle", 2, get_file_information_by_handle),
    export!("GetPrivateProfileIntA", 4, get_private_profile_int_a),
    export!("GetPrivateProfileStringA", 6, get_private_profile_string_a),
    export!("GetProcessAffinityMask", 3, get_process_affinity_mask),
    export!("GetProcessVersion", 1, get_process_version),
    export!("GetProfileIntA", 3, get_profile_int_a),
    export!("GetProfileStringA", 5, get_profile_string_a),
    export!("GetShortPathNameA", 3, get_short_path_name_a),
    export!("GetTimeFormatA", 6, get_time_format_a),
    export!("IsValidLocale", 2, is_valid_locale),
    export!("LockFile", 5, lock_file),
    export!("SetFileTime", 4, set_file_time),
    export!("UnlockFile", 5, lock_file),
    export!("WritePrivateProfileStringA", 4, write_private_profile_string_a),
    export!("WriteProfileStringA", 3, write_profile_string_a),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_edit() {
        let mut ini = Ini::parse(b"; c\r\n[A]\r\nx=1\r\n\r\n[B]\r\ny = two \r\n");
        assert_eq!(ini.get("a", "X").as_deref(), Some("1"));
        assert_eq!(ini.get("B", "y").as_deref(), Some("two"));
        ini.set("A", Some("z"), Some("3"));
        ini.set("B", Some("y"), None);
        ini.set("C", Some("k"), Some("v"));
        assert_eq!(ini.keys("A"), vec!["x", "z"]);
        assert!(ini.keys("B").is_empty());
        assert_eq!(ini.sections(), vec!["A", "B", "C"]);
        assert_eq!(ini.get("C", "k").as_deref(), Some("v"));
        assert_eq!(parse_profile_int(" 42abc"), Some(42));
        assert_eq!(parse_profile_int("0x10"), Some(16));
    }

    #[test]
    fn pictures() {
        let st = [1999, 10, 5, 29, 14, 7, 3, 0];
        assert_eq!(
            format_picture("dddd, MMMM dd, yyyy", &st, true),
            "Friday, October 29, 1999"
        );
        assert_eq!(format_picture("M/d/yy", &st, true), "10/29/99");
        assert_eq!(format_picture("h:mm:ss tt", &st, false), "2:07:03 PM");
        assert_eq!(format_picture("HH'h'mm", &st, false), "14h07");
    }
}
