//! Virtual filesystem: immutable host mounts plus a session-owned writable overlay.
//!
//! * Host directories are mounted read-only at guest path prefixes (e.g. `C:\GAME`).
//!   The installation is never modified.
//! * All guest writes go to an in-memory overlay (copy-up on first write). The overlay is
//!   part of checkpoints and can optionally be persisted to a host directory so that game
//!   saves/configuration survive runtime restarts.
//! * Name matching is case-insensitive; enumeration is sorted by upper-cased name and
//!   base-file timestamps come from the profile, so metadata is reproducible regardless
//!   of host filesystem order or copy times.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestPath {
    pub drive: char,
    pub comps: Vec<String>,
}

impl GuestPath {
    pub fn key(&self) -> String {
        let mut s = format!("{}:", self.drive);
        for c in &self.comps {
            s.push('\\');
            s.push_str(&c.to_uppercase());
        }
        s
    }
    pub fn display(&self) -> String {
        let mut s = format!("{}:", self.drive);
        if self.comps.is_empty() {
            s.push('\\');
        }
        for c in &self.comps {
            s.push('\\');
            s.push_str(c);
        }
        s
    }
    pub fn parent(&self) -> Option<GuestPath> {
        if self.comps.is_empty() {
            return None;
        }
        let mut p = self.clone();
        p.comps.pop();
        Some(p)
    }
    pub fn join(&self, name: &str) -> GuestPath {
        let mut p = self.clone();
        p.comps.push(name.to_string());
        p
    }
    pub fn file_name(&self) -> &str {
        self.comps.last().map(|s| s.as_str()).unwrap_or("")
    }
}

/// Resolve a Win32 path string against a current directory.
pub fn normalize(path: &str, cwd: &GuestPath) -> Option<GuestPath> {
    let p = path.replace('/', "\\");
    if p.starts_with("\\\\") {
        return None; // UNC / device paths are not supported
    }
    let bytes = p.as_bytes();
    let (drive, rest, absolute) = if bytes.len() >= 2 && bytes[1] == b':' {
        let d = (bytes[0] as char).to_ascii_uppercase();
        if !d.is_ascii_alphabetic() {
            return None;
        }
        let rest = &p[2..];
        (d, rest.to_string(), rest.starts_with('\\') || d != cwd.drive)
    } else if p.starts_with('\\') {
        (cwd.drive, p.clone(), true)
    } else {
        (cwd.drive, p.clone(), false)
    };
    let mut out: Vec<String> = if absolute { Vec::new() } else { cwd.comps.clone() };
    for c in rest.split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => {
                let t = c.trim_end_matches([' ', '.']);
                if !t.is_empty() {
                    out.push(t.to_string());
                }
            }
        }
    }
    Some(GuestPath { drive, comps: out })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mount {
    pub guest: GuestPath,
    pub host: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriveKind {
    Fixed,
    CdRom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drive {
    pub letter: char,
    pub kind: DriveKind,
    pub label: String,
    pub serial: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OverlayNode {
    File {
        name: String,
        data: Vec<u8>,
        attrs: u32,
        mtime: u64,
    },
    Dir {
        name: String,
        mtime: u64,
    },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub attrs: u32,
    pub size: u64,
    /// FILETIME (100 ns units since 1601).
    pub mtime: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Backing {
    Host(PathBuf),
    Overlay(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFile {
    pub path: GuestPath,
    pub backing: Backing,
    pub pos: u64,
    pub read: bool,
    pub write: bool,
    pub delete_on_close: bool,
    /// FILE_SHARE_* flags this handle was opened with.
    pub share: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    NotFound,
    PathNotFound,
    Exists,
    AccessDenied,
    IsDirectory,
    NotEmpty,
    Invalid,
}

impl VfsError {
    pub fn win32(self) -> u32 {
        match self {
            VfsError::NotFound => 2,
            VfsError::PathNotFound => 3,
            VfsError::AccessDenied | VfsError::IsDirectory => 5,
            VfsError::Exists => 183, // ERROR_ALREADY_EXISTS (CreateFile uses 80 for CREATE_NEW)
            VfsError::NotEmpty => 145,
            VfsError::Invalid => 123,
        }
    }
}

pub type VfsResult<T> = Result<T, VfsError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    CreateNew,
    CreateAlways,
    OpenExisting,
    OpenAlways,
    TruncateExisting,
}

enum Node {
    HostFile(PathBuf, u64),
    HostDir(PathBuf),
    VirtualDir,
    OverlayFile(String),
    OverlayDir,
}

#[derive(Default)]
struct HostCache {
    /// Upper-cased name → actual name, per host directory.
    dirs: HashMap<PathBuf, BTreeMap<String, String>>,
    files: HashMap<PathBuf, std::fs::File>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Vfs {
    pub mounts: Vec<Mount>,
    pub drives: BTreeMap<char, Drive>,
    pub overlay: BTreeMap<String, OverlayNode>,
    /// FILETIME reported for files from host mounts.
    pub base_file_time: u64,
    #[serde(skip)]
    pub persist_dir: Option<PathBuf>,
    #[serde(skip)]
    cache: HostCacheCell,
}

#[derive(Default)]
struct HostCacheCell(std::cell::RefCell<HostCache>);

impl Clone for HostCacheCell {
    /// The host lookup cache is rebuilt on demand rather than copied.
    fn clone(&self) -> Self {
        HostCacheCell::default()
    }
}

impl Vfs {
    pub fn new() -> Self {
        let mut drives = BTreeMap::new();
        drives.insert(
            'C',
            Drive {
                letter: 'C',
                kind: DriveKind::Fixed,
                label: String::new(),
                serial: 0x1234_5678,
            },
        );
        Vfs {
            mounts: Vec::new(),
            drives,
            overlay: BTreeMap::new(),
            // 2001-01-01 00:00:00 UTC
            base_file_time: 126_227_808_000_000_000,
            persist_dir: None,
            cache: HostCacheCell::default(),
        }
    }

    pub fn mount(&mut self, guest: GuestPath, host: PathBuf) {
        self.drives.entry(guest.drive).or_insert(Drive {
            letter: guest.drive,
            kind: DriveKind::Fixed,
            label: String::new(),
            serial: 0x1234_0000 | guest.drive as u32,
        });
        self.mounts.push(Mount { guest, host });
        // Longest prefix first.
        self.mounts.sort_by_key(|m| std::cmp::Reverse(m.guest.comps.len()));
    }

    fn host_lookup(&self, dir: &Path, name: &str) -> Option<String> {
        let mut c = self.cache.0.borrow_mut();
        let listing = c.dirs.entry(dir.to_path_buf()).or_insert_with(|| {
            let mut m = BTreeMap::new();
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    if let Some(n) = e.file_name().to_str() {
                        m.insert(n.to_uppercase(), n.to_string());
                    }
                }
            }
            m
        });
        listing.get(&name.to_uppercase()).cloned()
    }

    fn host_listing(&self, dir: &Path) -> BTreeMap<String, String> {
        let _ = self.host_lookup(dir, "");
        self.cache.0.borrow().dirs.get(dir).cloned().unwrap_or_default()
    }

    fn invalidate_host_cache(&self) {
        self.cache.0.borrow_mut().dirs.clear();
    }

    /// Resolve a path through mounts to a host path if it exists there.
    fn host_resolve(&self, p: &GuestPath) -> Option<(PathBuf, bool)> {
        for m in &self.mounts {
            if m.guest.drive != p.drive || p.comps.len() < m.guest.comps.len() {
                continue;
            }
            if !m
                .guest
                .comps
                .iter()
                .zip(&p.comps)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
            {
                continue;
            }
            let mut host = m.host.clone();
            for c in &p.comps[m.guest.comps.len()..] {
                let actual = self.host_lookup(&host, c)?;
                host.push(actual);
            }
            let md = std::fs::metadata(&host).ok()?;
            return Some((host, md.is_dir()));
        }
        None
    }

    /// Is `p` a strict ancestor of a mount point (an implicit virtual directory)?
    fn is_virtual_dir(&self, p: &GuestPath) -> bool {
        if p.comps.is_empty() {
            return self.drives.contains_key(&p.drive);
        }
        self.mounts.iter().any(|m| {
            m.guest.drive == p.drive
                && m.guest.comps.len() > p.comps.len()
                && m.guest
                    .comps
                    .iter()
                    .zip(&p.comps)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
    }

    fn node(&self, p: &GuestPath) -> Option<Node> {
        let key = p.key();
        if let Some(n) = self.overlay.get(&key) {
            return match n {
                OverlayNode::File { .. } => Some(Node::OverlayFile(key)),
                OverlayNode::Dir { .. } => Some(Node::OverlayDir),
                OverlayNode::Deleted => None,
            };
        }
        if let Some((h, is_dir)) = self.host_resolve(p) {
            if is_dir {
                return Some(Node::HostDir(h));
            }
            let len = std::fs::metadata(&h).map(|m| m.len()).unwrap_or(0);
            return Some(Node::HostFile(h, len));
        }
        if self.is_virtual_dir(p) {
            return Some(Node::VirtualDir);
        }
        None
    }

    pub fn exists_dir(&self, p: &GuestPath) -> bool {
        matches!(
            self.node(p),
            Some(Node::HostDir(_) | Node::VirtualDir | Node::OverlayDir)
        )
    }

    pub fn attributes(&self, p: &GuestPath) -> VfsResult<u32> {
        match self.node(p) {
            Some(Node::HostDir(_) | Node::VirtualDir | Node::OverlayDir) => Ok(FILE_ATTRIBUTE_DIRECTORY),
            Some(Node::HostFile(..)) => Ok(FILE_ATTRIBUTE_ARCHIVE),
            Some(Node::OverlayFile(k)) => match &self.overlay[&k] {
                OverlayNode::File { attrs, .. } => Ok(*attrs),
                _ => unreachable!(),
            },
            None => Err(self.missing(p)),
        }
    }

    pub fn stat(&self, p: &GuestPath) -> VfsResult<DirEntry> {
        let name = p.file_name().to_string();
        match self.node(p) {
            Some(Node::HostFile(_, len)) => Ok(DirEntry {
                name,
                attrs: FILE_ATTRIBUTE_ARCHIVE,
                size: len,
                mtime: self.base_file_time,
            }),
            Some(Node::OverlayFile(k)) => match &self.overlay[&k] {
                OverlayNode::File { data, attrs, mtime, .. } => Ok(DirEntry {
                    name,
                    attrs: *attrs,
                    size: data.len() as u64,
                    mtime: *mtime,
                }),
                _ => unreachable!(),
            },
            Some(_) => Ok(DirEntry {
                name,
                attrs: FILE_ATTRIBUTE_DIRECTORY,
                size: 0,
                mtime: self.base_file_time,
            }),
            None => Err(self.missing(p)),
        }
    }

    fn missing(&self, p: &GuestPath) -> VfsError {
        match p.parent() {
            Some(par) if !self.exists_dir(&par) => VfsError::PathNotFound,
            _ => VfsError::NotFound,
        }
    }

    pub fn open(
        &mut self,
        p: &GuestPath,
        read: bool,
        write: bool,
        disp: Disposition,
        now: u64,
    ) -> VfsResult<(OpenFile, bool)> {
        let parent_ok = p.parent().map(|par| self.exists_dir(&par)).unwrap_or(true);
        let node = self.node(p);
        let existed = node.is_some();
        if matches!(node, Some(Node::HostDir(_) | Node::VirtualDir | Node::OverlayDir)) {
            return Err(VfsError::IsDirectory);
        }
        match (disp, existed) {
            (Disposition::CreateNew, true) => return Err(VfsError::Exists),
            (Disposition::OpenExisting | Disposition::TruncateExisting, false) => {
                return Err(if parent_ok {
                    VfsError::NotFound
                } else {
                    VfsError::PathNotFound
                });
            }
            _ => {}
        }
        if !existed && !parent_ok {
            return Err(VfsError::PathNotFound);
        }
        let truncate = matches!(disp, Disposition::CreateAlways | Disposition::TruncateExisting) || (!existed);
        let key = p.key();
        let needs_overlay = write || truncate;
        let backing = if needs_overlay {
            if let Some(OverlayNode::File { attrs, .. }) = self.overlay.get(&key)
                && write
                && attrs & FILE_ATTRIBUTE_READONLY != 0
            {
                return Err(VfsError::AccessDenied);
            }
            let data = if truncate {
                Vec::new()
            } else {
                match &node {
                    Some(Node::HostFile(h, _)) => std::fs::read(h).map_err(|_| VfsError::AccessDenied)?,
                    Some(Node::OverlayFile(k)) => match &self.overlay[k] {
                        OverlayNode::File { data, .. } => data.clone(),
                        _ => unreachable!(),
                    },
                    _ => Vec::new(),
                }
            };
            let attrs = match self.overlay.get(&key) {
                Some(OverlayNode::File { attrs, .. }) => *attrs,
                _ => FILE_ATTRIBUTE_ARCHIVE,
            };
            // An existing file keeps the case of its name, as on Windows.
            let name = match (&node, self.overlay.get(&key)) {
                (_, Some(OverlayNode::File { name, .. })) => name.clone(),
                (Some(Node::HostFile(h, _)), _) => h
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.file_name().to_string()),
                _ => p.file_name().to_string(),
            };
            self.overlay.insert(
                key.clone(),
                OverlayNode::File {
                    name,
                    data,
                    attrs,
                    mtime: now,
                },
            );
            Backing::Overlay(key)
        } else {
            match node {
                Some(Node::HostFile(h, _)) => Backing::Host(h),
                Some(Node::OverlayFile(k)) => Backing::Overlay(k),
                _ => return Err(VfsError::NotFound),
            }
        };
        Ok((
            OpenFile {
                path: p.clone(),
                backing,
                pos: 0,
                read,
                write,
                delete_on_close: false,
                share: 3,
            },
            existed,
        ))
    }

    pub fn file_size(&self, f: &OpenFile) -> u64 {
        match &f.backing {
            Backing::Host(h) => std::fs::metadata(h).map(|m| m.len()).unwrap_or(0),
            Backing::Overlay(k) => match self.overlay.get(k) {
                Some(OverlayNode::File { data, .. }) => data.len() as u64,
                _ => 0,
            },
        }
    }

    pub fn read(&self, f: &mut OpenFile, out: &mut [u8]) -> VfsResult<usize> {
        if !f.read {
            return Err(VfsError::AccessDenied);
        }
        let n = match &f.backing {
            Backing::Host(h) => {
                let mut c = self.cache.0.borrow_mut();
                if !c.files.contains_key(h) {
                    let file = std::fs::File::open(h).map_err(|_| VfsError::AccessDenied)?;
                    c.files.insert(h.clone(), file);
                }
                let file = c.files.get_mut(h).unwrap();
                file.seek(SeekFrom::Start(f.pos)).map_err(|_| VfsError::Invalid)?;
                let mut done = 0;
                while done < out.len() {
                    let n = file.read(&mut out[done..]).map_err(|_| VfsError::Invalid)?;
                    if n == 0 {
                        break;
                    }
                    done += n;
                }
                done
            }
            Backing::Overlay(k) => match self.overlay.get(k) {
                Some(OverlayNode::File { data, .. }) => {
                    let start = (f.pos as usize).min(data.len());
                    let n = out.len().min(data.len() - start);
                    out[..n].copy_from_slice(&data[start..start + n]);
                    n
                }
                _ => 0,
            },
        };
        f.pos += n as u64;
        Ok(n)
    }

    pub fn write(&mut self, f: &mut OpenFile, data_in: &[u8], now: u64) -> VfsResult<usize> {
        if !f.write {
            return Err(VfsError::AccessDenied);
        }
        let Backing::Overlay(k) = &f.backing else {
            return Err(VfsError::AccessDenied);
        };
        match self.overlay.get_mut(k) {
            Some(OverlayNode::File { data, mtime, .. }) => {
                let pos = f.pos as usize;
                if data.len() < pos + data_in.len() {
                    data.resize(pos + data_in.len(), 0);
                }
                data[pos..pos + data_in.len()].copy_from_slice(data_in);
                *mtime = now;
            }
            _ => return Err(VfsError::NotFound),
        }
        f.pos += data_in.len() as u64;
        Ok(data_in.len())
    }

    pub fn set_len(&mut self, f: &OpenFile, len: u64) -> VfsResult<()> {
        let Backing::Overlay(k) = &f.backing else {
            return Err(VfsError::AccessDenied);
        };
        if let Some(OverlayNode::File { data, .. }) = self.overlay.get_mut(k) {
            data.resize(len as usize, 0);
        }
        Ok(())
    }

    pub fn delete(&mut self, p: &GuestPath) -> VfsResult<()> {
        match self.node(p) {
            Some(Node::HostFile(..) | Node::OverlayFile(_)) => {
                self.overlay.insert(p.key(), OverlayNode::Deleted);
                Ok(())
            }
            Some(_) => Err(VfsError::AccessDenied),
            None => Err(self.missing(p)),
        }
    }

    pub fn mkdir(&mut self, p: &GuestPath, now: u64) -> VfsResult<()> {
        if self.node(p).is_some() {
            return Err(VfsError::Exists);
        }
        match p.parent() {
            Some(par) if self.exists_dir(&par) => {}
            _ => return Err(VfsError::PathNotFound),
        }
        self.overlay.insert(
            p.key(),
            OverlayNode::Dir {
                name: p.file_name().to_string(),
                mtime: now,
            },
        );
        Ok(())
    }

    pub fn rmdir(&mut self, p: &GuestPath) -> VfsResult<()> {
        if !self.exists_dir(p) {
            return Err(self.missing(p));
        }
        if self.list_dir(p).iter().any(|e| e.name != "." && e.name != "..") {
            return Err(VfsError::NotEmpty);
        }
        self.overlay.insert(p.key(), OverlayNode::Deleted);
        Ok(())
    }

    pub fn rename(&mut self, from: &GuestPath, to: &GuestPath, now: u64) -> VfsResult<()> {
        if self.node(to).is_some() {
            return Err(VfsError::Exists);
        }
        let (mut f, _) = self.open(from, true, false, Disposition::OpenExisting, now)?;
        let len = self.file_size(&f) as usize;
        let mut buf = vec![0u8; len];
        self.read(&mut f, &mut buf)?;
        let attrs = self.attributes(from)?;
        self.overlay.insert(
            to.key(),
            OverlayNode::File {
                name: to.file_name().to_string(),
                data: buf,
                attrs,
                mtime: now,
            },
        );
        self.overlay.insert(from.key(), OverlayNode::Deleted);
        Ok(())
    }

    /// List a directory (including "." and ".." for non-root directories), sorted.
    pub fn list_dir(&self, p: &GuestPath) -> Vec<DirEntry> {
        let mut entries: BTreeMap<String, DirEntry> = BTreeMap::new();
        // Host contents.
        if let Some((h, true)) = self.host_resolve(p) {
            for (up, actual) in self.host_listing(&h) {
                let hp = h.join(&actual);
                if let Ok(md) = std::fs::metadata(&hp) {
                    entries.insert(
                        up,
                        DirEntry {
                            name: actual,
                            attrs: if md.is_dir() {
                                FILE_ATTRIBUTE_DIRECTORY
                            } else {
                                FILE_ATTRIBUTE_ARCHIVE
                            },
                            size: if md.is_dir() { 0 } else { md.len() },
                            mtime: self.base_file_time,
                        },
                    );
                }
            }
        }
        // Mount points directly below p.
        for m in &self.mounts {
            if m.guest.drive == p.drive
                && m.guest.comps.len() == p.comps.len() + 1
                && m.guest
                    .comps
                    .iter()
                    .zip(&p.comps)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
            {
                let n = m.guest.comps.last().unwrap().clone();
                entries.insert(
                    n.to_uppercase(),
                    DirEntry {
                        name: n,
                        attrs: FILE_ATTRIBUTE_DIRECTORY,
                        size: 0,
                        mtime: self.base_file_time,
                    },
                );
            }
        }
        // Overlay children.
        let prefix = format!("{}\\", p.key());
        for (k, n) in self.overlay.range(prefix.clone()..) {
            if !k.starts_with(&prefix) {
                break;
            }
            let rest = &k[prefix.len()..];
            if rest.contains('\\') {
                continue;
            }
            match n {
                OverlayNode::Deleted => {
                    entries.remove(rest);
                }
                OverlayNode::File {
                    name,
                    data,
                    attrs,
                    mtime,
                } => {
                    entries.insert(
                        rest.to_string(),
                        DirEntry {
                            name: name.clone(),
                            attrs: *attrs,
                            size: data.len() as u64,
                            mtime: *mtime,
                        },
                    );
                }
                OverlayNode::Dir { name, mtime } => {
                    entries.insert(
                        rest.to_string(),
                        DirEntry {
                            name: name.clone(),
                            attrs: FILE_ATTRIBUTE_DIRECTORY,
                            size: 0,
                            mtime: *mtime,
                        },
                    );
                }
            }
        }
        let mut out = Vec::new();
        if !p.comps.is_empty() {
            for dot in [".", ".."] {
                out.push(DirEntry {
                    name: dot.into(),
                    attrs: FILE_ATTRIBUTE_DIRECTORY,
                    size: 0,
                    mtime: self.base_file_time,
                });
            }
        }
        out.extend(entries.into_values());
        out
    }

    /// Load a persisted overlay from `dir` (files under `<dir>/<drive>/...`).
    pub fn load_persisted(&mut self, dir: &Path) -> std::io::Result<()> {
        self.persist_dir = Some(dir.to_path_buf());
        let manifest = dir.join("overlay.json");
        if manifest.exists() {
            let m: BTreeMap<String, OverlayNode> =
                serde_json::from_slice(&std::fs::read(&manifest)?).map_err(|e| std::io::Error::other(e.to_string()))?;
            for (k, n) in m {
                let n = match n {
                    OverlayNode::File { name, attrs, mtime, .. } => {
                        let data = std::fs::read(dir.join("files").join(hex::encode(&k)))?;
                        OverlayNode::File {
                            name,
                            data,
                            attrs,
                            mtime,
                        }
                    }
                    other => other,
                };
                self.overlay.insert(k, n);
            }
        }
        Ok(())
    }

    /// Write the overlay to the persistence directory (if configured).
    pub fn persist(&self) -> std::io::Result<()> {
        let Some(dir) = &self.persist_dir else {
            return Ok(());
        };
        let files = dir.join("files");
        std::fs::create_dir_all(&files)?;
        let mut manifest = BTreeMap::new();
        for (k, n) in &self.overlay {
            let entry = match n {
                OverlayNode::File {
                    name,
                    data,
                    attrs,
                    mtime,
                } => {
                    std::fs::write(files.join(hex::encode(k)), data)?;
                    OverlayNode::File {
                        name: name.clone(),
                        data: Vec::new(),
                        attrs: *attrs,
                        mtime: *mtime,
                    }
                }
                other => other.clone(),
            };
            manifest.insert(k.clone(), entry);
        }
        std::fs::write(
            dir.join("overlay.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|e| std::io::Error::other(e.to_string()))?,
        )?;
        self.invalidate_host_cache();
        Ok(())
    }
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

/// Win32 wildcard matching (`*`, `?`), case-insensitive. `*.*` matches names without dots.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_uppercase().chars().collect();
    let n: Vec<char> = name.to_uppercase().chars().collect();
    if pattern == "*.*" || pattern == "*" {
        return true;
    }
    fn m(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], n) || (!n.is_empty() && m(p, &n[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &n[1..]),
            // A trailing ".*" or "?" may match nothing at the end of the name.
            (Some('?'), None) => m(&p[1..], n),
            (Some('.'), None) => m(&p[1..], n),
            (Some(a), Some(b)) if a == b => m(&p[1..], &n[1..]),
            _ => false,
        }
    }
    m(&p, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> GuestPath {
        GuestPath {
            drive: 'C',
            comps: vec!["Game".into()],
        }
    }

    #[test]
    fn normalize_paths() {
        let c = cwd();
        assert_eq!(normalize("data\\x.txt", &c).unwrap().display(), "C:\\Game\\data\\x.txt");
        assert_eq!(normalize("..\\x", &c).unwrap().display(), "C:\\x");
        assert_eq!(normalize("\\a/b", &c).unwrap().display(), "C:\\a\\b");
        assert_eq!(normalize("d:\\a\\.\\b.", &c).unwrap().display(), "D:\\a\\b");
        assert_eq!(normalize(".", &c).unwrap().display(), "C:\\Game");
        assert_eq!(normalize("C:\\", &c).unwrap().display(), "C:\\");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.txt", "A.TXT"));
        assert!(!wildcard_match("*.txt", "a.txtx"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*.*", "noext"));
        assert!(wildcard_match("player*.cfg", "Player0.cfg"));
    }

    #[test]
    fn overlay_copy_up_and_listing() {
        let dir = std::env::temp_dir().join(format!("drydock-vfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub")).unwrap();
        std::fs::write(dir.join("Base.TXT"), b"hello").unwrap();
        let mut v = Vfs::new();
        v.mount(normalize("C:\\Game", &cwd()).unwrap(), dir.clone());
        let p = normalize("C:\\GAME\\base.txt", &cwd()).unwrap();
        let (mut f, existed) = v.open(&p, true, false, Disposition::OpenExisting, 0).unwrap();
        assert!(existed);
        let mut b = [0u8; 16];
        assert_eq!(v.read(&mut f, &mut b).unwrap(), 5);
        // Writing copies up; the host file is untouched.
        let (mut w, _) = v.open(&p, true, true, Disposition::OpenExisting, 0).unwrap();
        w.pos = 5;
        v.write(&mut w, b" world", 1).unwrap();
        assert_eq!(std::fs::read(dir.join("Base.TXT")).unwrap(), b"hello");
        let names: Vec<_> = v
            .list_dir(&normalize("C:\\Game", &cwd()).unwrap())
            .into_iter()
            .map(|e| (e.name, e.size))
            .collect();
        assert_eq!(
            names,
            vec![
                (".".into(), 0),
                ("..".into(), 0),
                ("Base.TXT".into(), 11),
                ("Sub".into(), 0)
            ]
        );
        v.delete(&p).unwrap();
        assert!(v.attributes(&p).is_err());
        assert_eq!(v.list_dir(&normalize("C:\\", &cwd()).unwrap())[0].name, "Game");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
