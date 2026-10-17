//! kernel32: files, directories, drives, environment.

use super::kernel32::*;
use crate::api::*;
use crate::objects::{FindState, KObj};
use crate::session::Session;
use crate::vfs::{self, Disposition, GuestPath, VfsError};

pub const INVALID_HANDLE_VALUE: u32 = 0xffff_ffff;

impl Session {
    pub fn cwd(&self) -> GuestPath {
        self.process.cwd.clone().unwrap_or(GuestPath {
            drive: 'C',
            comps: vec![],
        })
    }
    pub fn guest_path(&self, s: &str) -> Option<GuestPath> {
        vfs::normalize(s, &self.cwd())
    }
}

fn vfs_err(s: &mut Session, e: VfsError) {
    s.set_last_error(e.win32());
}

fn is_device_name(name: &str) -> bool {
    let b = crate::loader::basename(name).to_uppercase();
    let stem = b.split('.').next().unwrap_or("");
    matches!(stem, "CON" | "NUL" | "PRN" | "AUX" | "COM1" | "COM2" | "LPT1") || name.starts_with("\\\\.\\")
}

pub fn create_file_a(s: &mut Session) -> ApiResult {
    let [name, access, share, _sa, disp, flags, _tmpl] = s.args::<7>()?;
    let name = s.read_str(name)?;
    if is_device_name(&name) {
        s.missing("CreateFile", format!("device path {name:?} is not supported"));
        s.set_last_error(ERROR_FILE_NOT_FOUND);
        return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
    }
    let Some(p) = s.guest_path(&name) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
    };
    let disp = match disp {
        1 => Disposition::CreateNew,
        2 => Disposition::CreateAlways,
        3 => Disposition::OpenExisting,
        4 => Disposition::OpenAlways,
        5 => Disposition::TruncateExisting,
        _ => {
            s.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
        }
    };
    let read = access & 0x8000_0000 != 0 || access & 0x1 != 0 || access & 0x1000_0000 != 0;
    let write = access & 0x4000_0000 != 0 || access & 0x2 != 0 || access & 0x1000_0000 != 0;
    // Sharing: refuse if an existing open of the same file conflicts.
    let key = p.key();
    for e in s.objects.objs.values() {
        if let KObj::File(f) = &e.obj
            && f.path.key() == key
        {
            let conflict = (write && f.share & 2 == 0)
                || (read && f.share & 1 == 0)
                || (f.write && share & 2 == 0)
                || (f.read && share & 1 == 0);
            if conflict {
                s.set_last_error(32); // ERROR_SHARING_VIOLATION
                return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
            }
        }
    }
    let now = s.filetime_now();
    match s.vfs.open(&p, read, write, disp, now) {
        Ok((mut f, existed)) => {
            f.share = share;
            f.delete_on_close = flags & 0x0400_0000 != 0;
            let (h, _) = s.objects.create_handle(KObj::File(f), None);
            let le = if existed && matches!(disp, Disposition::CreateAlways | Disposition::OpenAlways) {
                ERROR_ALREADY_EXISTS
            } else {
                0
            };
            s.set_last_error(le);
            Ok(ApiRet::Ret(h))
        }
        Err(VfsError::Exists) => {
            s.set_last_error(80); // ERROR_FILE_EXISTS
            Ok(ApiRet::Ret(INVALID_HANDLE_VALUE))
        }
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(INVALID_HANDLE_VALUE))
        }
    }
}

fn with_file<T>(
    s: &mut Session,
    h: u32,
    f: impl FnOnce(&mut Session, &mut vfs::OpenFile) -> T,
) -> Option<T> {
    let id = s.objects.lookup(h)?;
    let mut file = match s.objects.get(id) {
        Some(KObj::File(f)) => f.clone(),
        _ => return None,
    };
    let r = f(s, &mut file);
    if let Some(KObj::File(slot)) = s.objects.get_mut(id) {
        *slot = file;
    }
    Some(r)
}

pub fn read_file(s: &mut Session, ) -> ApiResult {
    let [h, buf, n, pread, ovl] = s.args::<5>()?;
    if ovl != 0 {
        s.missing("ReadFile", "overlapped I/O".into());
    }
    if pread != 0 {
        s.mem.write_u32(pread, 0)?;
    }
    if !s.mem.is_writable(buf, n) {
        s.set_last_error(998); // ERROR_NOACCESS
        return Ok(ApiRet::Ret(0));
    }
    let r = with_file(s, h, |s, f| {
        let mut done = 0u32;
        let mut chunk = vec![0u8; (n as usize).min(1 << 20)];
        while done < n {
            let want = ((n - done) as usize).min(chunk.len());
            match s.vfs.read(f, &mut chunk[..want]) {
                Ok(0) => break,
                Ok(k) => {
                    if let Err(e) = s.mem.write(buf + done, &chunk[..k]) {
                        return Err(Err(e));
                    }
                    done += k as u32;
                }
                Err(e) => return Err(Ok(e)),
            }
        }
        Ok(done)
    });
    match r {
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
        Some(Ok(done)) => {
            if pread != 0 {
                s.mem.write_u32(pread, done)?;
            }
            Ok(ApiRet::Ret(1))
        }
        Some(Err(Ok(e))) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
        Some(Err(Err(e))) => Err(e.into()),
    }
}

pub fn write_file(s: &mut Session) -> ApiResult {
    let [h, buf, n, pwritten, _ovl] = s.args::<5>()?;
    let data = s.mem.read_vec(buf, n)?;
    if let Some(KObj::Console { which }) = s.objects.by_handle(h) {
        let w = *which;
        s.diag.debug_string(format!("[std{w}] {}", String::from_utf8_lossy(&data)));
        if pwritten != 0 {
            s.mem.write_u32(pwritten, n)?;
        }
        return Ok(ApiRet::Ret(1));
    }
    let now = s.filetime_now();
    let r = with_file(s, h, |s, f| s.vfs.write(f, &data, now));
    match r {
        Some(Ok(k)) => {
            if pwritten != 0 {
                s.mem.write_u32(pwritten, k as u32)?;
            }
            Ok(ApiRet::Ret(1))
        }
        Some(Err(e)) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn set_file_pointer(s: &mut Session) -> ApiResult {
    let [h, lo, phi, method] = s.args::<4>()?;
    let dist: i64 = if phi != 0 {
        ((s.mem.read_u32(phi)? as i64) << 32) | lo as i64
    } else {
        lo as i32 as i64
    };
    let r = with_file(s, h, |s, f| {
        let base = match method {
            0 => 0i64,
            1 => f.pos as i64,
            2 => s.vfs.file_size(f) as i64,
            _ => return Err(ERROR_INVALID_PARAMETER),
        };
        let np = base + dist;
        if np < 0 {
            return Err(131); // ERROR_NEGATIVE_SEEK
        }
        f.pos = np as u64;
        Ok(np as u64)
    });
    match r {
        Some(Ok(np)) => {
            if phi != 0 {
                s.mem.write_u32(phi, (np >> 32) as u32)?;
            }
            s.set_last_error(0);
            Ok(ApiRet::Ret(np as u32))
        }
        Some(Err(e)) => {
            s.set_last_error(e);
            Ok(ApiRet::Ret(0xffff_ffff))
        }
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0xffff_ffff))
        }
    }
}

pub fn get_file_size(s: &mut Session) -> ApiResult {
    let [h, phi] = s.args::<2>()?;
    match with_file(s, h, |s, f| s.vfs.file_size(f)) {
        Some(n) => {
            if phi != 0 {
                s.mem.write_u32(phi, (n >> 32) as u32)?;
            }
            Ok(ApiRet::Ret(n as u32))
        }
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0xffff_ffff))
        }
    }
}

pub fn get_file_time(s: &mut Session) -> ApiResult {
    let [h, c, a, w] = s.args::<4>()?;
    let Some(path) = with_file(s, h, |_, f| f.path.clone()) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(0));
    };
    let st = s.vfs.stat(&path).map(|e| e.mtime).unwrap_or(0);
    for p in [c, a, w] {
        if p != 0 {
            s.mem.write_u64(p, st)?;
        }
    }
    Ok(ApiRet::Ret(1))
}

pub fn set_end_of_file(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match with_file(s, h, |s, f| {
        let pos = f.pos;
        s.vfs.set_len(f, pos)
    }) {
        Some(Ok(())) => Ok(ApiRet::Ret(1)),
        Some(Err(e)) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn flush_file_buffers(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.objects.by_handle(h) {
        Some(KObj::File(_) | KObj::Console { .. }) => Ok(ApiRet::Ret(1)),
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn get_file_type(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.objects.by_handle(h) {
        Some(KObj::File(_)) => Ok(ApiRet::Ret(1)),
        Some(KObj::Console { .. }) => Ok(ApiRet::Ret(2)),
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

/// GUI processes started without a console have no standard handles.
pub fn get_std_handle(s: &mut Session) -> ApiResult {
    let [which] = s.args::<1>()?;
    let idx = match which {
        0xffff_fff6 => 0,
        0xffff_fff5 => 1,
        0xffff_fff4 => 2,
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
        }
    };
    Ok(ApiRet::Ret(s.process.std_handles[idx]))
}

pub fn set_std_handle(s: &mut Session) -> ApiResult {
    let [which, h] = s.args::<2>()?;
    let idx = match which {
        0xffff_fff6 => 0,
        0xffff_fff5 => 1,
        0xffff_fff4 => 2,
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(ApiRet::Ret(0));
        }
    };
    s.process.std_handles[idx] = h;
    Ok(ApiRet::Ret(1))
}

pub fn set_handle_count(s: &mut Session) -> ApiResult {
    let [n] = s.args::<1>()?;
    Ok(ApiRet::Ret(n))
}

pub fn get_file_attributes_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    match s.vfs.attributes(&gp) {
        Ok(a) => Ok(ApiRet::Ret(a)),
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0xffff_ffff))
        }
    }
}

pub fn set_file_attributes_a(s: &mut Session) -> ApiResult {
    let [p, attrs] = s.args::<2>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    let now = s.filetime_now();
    match s.vfs.open(&gp, true, true, Disposition::OpenExisting, now) {
        Ok(_) => {
            if let Some(vfs::OverlayNode::File { attrs: a, .. }) = s.vfs.overlay.get_mut(&gp.key()) {
                *a = if attrs == 0 { vfs::FILE_ATTRIBUTE_NORMAL } else { attrs };
            }
            Ok(ApiRet::Ret(1))
        }
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn delete_file_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    match s.vfs.delete(&gp) {
        Ok(()) => Ok(ApiRet::Ret(1)),
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn create_directory_a(s: &mut Session) -> ApiResult {
    let [p, _sa] = s.args::<2>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    let now = s.filetime_now();
    match s.vfs.mkdir(&gp, now) {
        Ok(()) => Ok(ApiRet::Ret(1)),
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn remove_directory_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    match s.vfs.rmdir(&gp) {
        Ok(()) => Ok(ApiRet::Ret(1)),
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn move_file_a(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    let (a, b) = (s.read_str(a)?, s.read_str(b)?);
    let (Some(pa), Some(pb)) = (s.guest_path(&a), s.guest_path(&b)) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    let now = s.filetime_now();
    match s.vfs.rename(&pa, &pb, now) {
        Ok(()) => Ok(ApiRet::Ret(1)),
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn copy_file_a(s: &mut Session) -> ApiResult {
    let [a, b, fail_if_exists] = s.args::<3>()?;
    let (a, b) = (s.read_str(a)?, s.read_str(b)?);
    let (Some(pa), Some(pb)) = (s.guest_path(&a), s.guest_path(&b)) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    let now = s.filetime_now();
    let r = (|| {
        let (mut src, _) = s.vfs.open(&pa, true, false, Disposition::OpenExisting, now)?;
        let n = s.vfs.file_size(&src) as usize;
        let mut buf = vec![0u8; n];
        s.vfs.read(&mut src, &mut buf)?;
        let disp = if fail_if_exists != 0 {
            Disposition::CreateNew
        } else {
            Disposition::CreateAlways
        };
        let (mut dst, _) = s.vfs.open(&pb, false, true, disp, now)?;
        s.vfs.write(&mut dst, &buf, now)?;
        Ok::<(), VfsError>(())
    })();
    match r {
        Ok(()) => Ok(ApiRet::Ret(1)),
        Err(VfsError::Exists) => {
            s.set_last_error(80);
            Ok(ApiRet::Ret(0))
        }
        Err(e) => {
            vfs_err(s, e);
            Ok(ApiRet::Ret(0))
        }
    }
}

// Find files
fn short_name(name: &str) -> String {
    let up = name.to_uppercase();
    let (stem, ext) = match up.rsplit_once('.') {
        Some((a, b)) if !a.is_empty() => (a.to_string(), b.to_string()),
        _ => (up.clone(), String::new()),
    };
    let valid = |c: char| c.is_ascii_alphanumeric() || "!#$%&'()-@^_`{}~".contains(c);
    if stem.len() <= 8 && ext.len() <= 3 && stem.chars().all(valid) && ext.chars().all(valid) {
        return String::new(); // already a valid 8.3 name
    }
    let s: String = stem.chars().filter(|&c| valid(c)).take(6).collect();
    let e: String = ext.chars().filter(|&c| valid(c)).take(3).collect();
    if e.is_empty() {
        format!("{s}~1")
    } else {
        format!("{s}~1.{e}")
    }
}

fn write_find_data(s: &mut Session, p: u32, e: &vfs::DirEntry) -> Result<(), ApiError> {
    let mut b = vec![0u8; 320];
    let put = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0, e.attrs);
    for o in [4usize, 12, 20] {
        b[o..o + 8].copy_from_slice(&e.mtime.to_le_bytes());
    }
    put(&mut b, 28, (e.size >> 32) as u32);
    put(&mut b, 32, e.size as u32);
    let n = crate::strings::encode_ansi(&e.name);
    let k = n.len().min(259);
    b[44..44 + k].copy_from_slice(&n[..k]);
    let sn = short_name(&e.name);
    let sk = sn.len().min(13);
    b[304..304 + sk].copy_from_slice(&sn.as_bytes()[..sk]);
    s.mem.write(p, &b)?;
    Ok(())
}

pub fn find_first_file_a(s: &mut Session) -> ApiResult {
    let [p, data] = s.args::<2>()?;
    let pattern = s.read_str(p)?;
    let Some(gp) = s.guest_path(&pattern) else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
    };
    let (dir, pat) = match gp.parent() {
        Some(d) => (d, gp.file_name().to_string()),
        None => {
            s.set_last_error(ERROR_FILE_NOT_FOUND);
            return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
        }
    };
    if !s.vfs.exists_dir(&dir) {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
    }
    let mut entries: Vec<vfs::DirEntry> = s
        .vfs
        .list_dir(&dir)
        .into_iter()
        .filter(|e| vfs::wildcard_match(&pat, &e.name))
        .collect();
    if entries.is_empty() {
        s.set_last_error(ERROR_FILE_NOT_FOUND);
        return Ok(ApiRet::Ret(INVALID_HANDLE_VALUE));
    }
    let first = entries.remove(0);
    write_find_data(s, data, &first)?;
    let (h, _) = s.objects.create_handle(KObj::Find(FindState { entries }), None);
    Ok(ApiRet::Ret(h))
}

pub fn find_next_file_a(s: &mut Session) -> ApiResult {
    let [h, data] = s.args::<2>()?;
    let next = match s.objects.by_handle_mut(h) {
        Some(KObj::Find(f)) => {
            if f.entries.is_empty() {
                None
            } else {
                Some(f.entries.remove(0))
            }
        }
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(ApiRet::Ret(0));
        }
    };
    match next {
        Some(e) => {
            write_find_data(s, data, &e)?;
            Ok(ApiRet::Ret(1))
        }
        None => {
            s.set_last_error(18); // ERROR_NO_MORE_FILES
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn find_close(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.objects.by_handle(h) {
        Some(KObj::Find(_)) => {
            let _ = s.objects.close(h);
            Ok(ApiRet::Ret(1))
        }
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

// Directories & paths
fn ret_path(s: &mut Session, buf: u32, n: u32, path: &str) -> ApiResult {
    match s.write_str_buf(buf, n, path)? {
        Ok(len) => Ok(ApiRet::Ret(len)),
        Err(need) => Ok(ApiRet::Ret(need)),
    }
}

pub fn get_current_directory_a(s: &mut Session) -> ApiResult {
    let [n, buf] = s.args::<2>()?;
    let d = s.cwd().display();
    ret_path(s, buf, n, &d)
}

pub fn set_current_directory_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_str(p)?;
    match s.guest_path(&name) {
        Some(gp) if s.vfs.exists_dir(&gp) => {
            s.process.cwd = Some(gp);
            Ok(ApiRet::Ret(1))
        }
        _ => {
            s.set_last_error(3);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn get_full_path_name_a(s: &mut Session) -> ApiResult {
    let [p, n, buf, filepart] = s.args::<4>()?;
    let name = s.read_str(p)?;
    let Some(gp) = s.guest_path(&name) else {
        s.set_last_error(123);
        return Ok(ApiRet::Ret(0));
    };
    let full = gp.display();
    let r = ret_path(s, buf, n, &full)?;
    if let ApiRet::Ret(len) = r
        && len < n
        && filepart != 0
    {
        let fname_off = full.rfind('\\').map(|i| i + 1).unwrap_or(0) as u32;
        let fp = if fname_off as usize >= full.len() {
            0
        } else {
            buf + fname_off
        };
        s.mem.write_u32(filepart, fp)?;
    }
    Ok(r)
}

pub fn get_system_directory_a(s: &mut Session) -> ApiResult {
    let [buf, n] = s.args::<2>()?;
    let d = s.config.windows.system_dir.clone();
    ret_path(s, buf, n, &d)
}
pub fn get_windows_directory_a(s: &mut Session) -> ApiResult {
    let [buf, n] = s.args::<2>()?;
    let d = s.config.windows.windows_dir.clone();
    ret_path(s, buf, n, &d)
}
pub fn get_temp_path_a(s: &mut Session) -> ApiResult {
    let [n, buf] = s.args::<2>()?;
    let d = format!("{}\\", s.config.windows.temp_dir.trim_end_matches('\\'));
    ret_path(s, buf, n, &d)
}

fn drive_of(s: &Session, root: u32) -> Result<Option<char>, ApiError> {
    if root == 0 {
        return Ok(Some(s.cwd().drive));
    }
    let r = s.read_str(root)?;
    Ok(r.chars().next().map(|c| c.to_ascii_uppercase()))
}

pub fn get_drive_type_a(s: &mut Session) -> ApiResult {
    let [root] = s.args::<1>()?;
    let d = drive_of(s, root)?;
    Ok(ApiRet::Ret(match d.and_then(|d| s.vfs.drives.get(&d)) {
        Some(drv) => match drv.kind {
            vfs::DriveKind::Fixed => 3,
            vfs::DriveKind::CdRom => 5,
        },
        None => 1, // DRIVE_NO_ROOT_DIR
    }))
}

pub fn get_logical_drives(s: &mut Session) -> ApiResult {
    let mut m = 0u32;
    for d in s.vfs.drives.keys() {
        m |= 1 << (*d as u32 - 'A' as u32);
    }
    Ok(ApiRet::Ret(m))
}

pub fn get_disk_free_space_a(s: &mut Session) -> ApiResult {
    let [root, spc, bps, free, total] = s.args::<5>()?;
    let d = drive_of(s, root)?;
    if d.and_then(|d| s.vfs.drives.get(&d)).is_none() {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    }
    // A 4 GiB volume with 4 KiB clusters, of which the overlay contents are in use.
    let used: u64 = s
        .vfs
        .overlay
        .values()
        .map(|n| match n {
            vfs::OverlayNode::File { data, .. } => data.len() as u64,
            _ => 0,
        })
        .sum();
    let total_clusters: u32 = 1 << 20;
    let free_clusters = total_clusters / 2 - (used.div_ceil(4096) as u32).min(total_clusters / 2);
    for (p, v) in [(spc, 8u32), (bps, 512), (free, free_clusters), (total, total_clusters)] {
        if p != 0 {
            s.mem.write_u32(p, v)?;
        }
    }
    Ok(ApiRet::Ret(1))
}

pub fn get_volume_information_a(s: &mut Session) -> ApiResult {
    let [root, vname, vn, serial, maxcomp, flags, fsname, fsn] = s.args::<8>()?;
    let d = drive_of(s, root)?;
    let Some(drv) = d.and_then(|d| s.vfs.drives.get(&d)).cloned() else {
        s.set_last_error(3);
        return Ok(ApiRet::Ret(0));
    };
    if vname != 0 {
        s.write_str_trunc(vname, vn, &drv.label)?;
    }
    if serial != 0 {
        s.mem.write_u32(serial, drv.serial)?;
    }
    if maxcomp != 0 {
        s.mem.write_u32(maxcomp, 255)?;
    }
    let (fl, fs) = match drv.kind {
        vfs::DriveKind::Fixed => (0x3, "NTFS"),
        vfs::DriveKind::CdRom => (0x80005, "CDFS"),
    };
    if flags != 0 {
        s.mem.write_u32(flags, fl)?;
    }
    if fsname != 0 {
        s.write_str_trunc(fsname, fsn, fs)?;
    }
    Ok(ApiRet::Ret(1))
}

// Environment
impl Session {
    fn env_block(&self, wide: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in &self.process.env {
            let line = format!("{k}={v}");
            if wide {
                out.extend(line.encode_utf16().flat_map(|c| c.to_le_bytes()));
                out.extend([0, 0]);
            } else {
                out.extend(crate::strings::encode_ansi(&line));
                out.push(0);
            }
        }
        if wide {
            out.extend([0, 0]);
        } else {
            out.push(0);
        }
        out
    }
}

pub fn get_environment_strings_a(s: &mut Session) -> ApiResult {
    let b = s.env_block(false);
    let p = s.sys_alloc(b.len() as u32)?;
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(p))
}
pub fn get_environment_strings_w(s: &mut Session) -> ApiResult {
    let b = s.env_block(true);
    let p = s.sys_alloc(b.len() as u32)?;
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(p))
}
pub fn free_environment_strings(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let sh = s.process.sys_heap;
    s.heaps.free(sh, p);
    Ok(ApiRet::Ret(1))
}
pub fn get_environment_variable_a(s: &mut Session) -> ApiResult {
    let [name, buf, n] = s.args::<3>()?;
    let name = s.read_str(name)?;
    match s
        .process
        .env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&name))
        .map(|(_, v)| v.clone())
    {
        Some(v) => match s.write_str_buf(buf, n, &v)? {
            Ok(len) => Ok(ApiRet::Ret(len)),
            Err(need) => Ok(ApiRet::Ret(need)),
        },
        None => {
            s.set_last_error(203); // ERROR_ENVVAR_NOT_FOUND
            Ok(ApiRet::Ret(0))
        }
    }
}
pub fn set_environment_variable_a(s: &mut Session) -> ApiResult {
    let [name, val] = s.args::<2>()?;
    let name = s.read_str(name)?;
    let val = s.read_str_opt(val)?;
    s.process.env.retain(|(k, _)| !k.eq_ignore_ascii_case(&name));
    if let Some(v) = val {
        s.process.env.push((name, v));
        s.process.env.sort_by_key(|(k, _)| k.to_uppercase());
    }
    Ok(ApiRet::Ret(1))
}
