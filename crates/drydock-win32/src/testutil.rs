//! Helpers for unit tests: sessions booted from generated fixture executables.

use crate::reg::Registry;
use crate::session::{Session, SessionConfig, WindowsProfile};
use crate::vfs::{self, Disposition, GuestPath, Vfs};

/// A session whose executable `C:\TEST\T.EXE` is `pe`, stored in the in-memory overlay.
pub fn session_with(pe: &[u8]) -> Session {
    let mut v = Vfs::new();
    let root = GuestPath {
        drive: 'C',
        comps: vec![],
    };
    let dir = vfs::normalize("C:\\TEST", &root).unwrap();
    v.mkdir(&dir, 0).unwrap();
    let exe = dir.join("T.EXE");
    let (mut f, _) = v.open(&exe, false, true, Disposition::CreateAlways, 0).unwrap();
    v.write(&mut f, pe, 0).unwrap();
    let config = SessionConfig {
        exe: exe.display(),
        args: String::new(),
        cwd: None,
        windows: WindowsProfile::default(),
        cpu: Default::default(),
        env: Vec::new(),
    };
    Session::new(config, v, Registry::new()).unwrap()
}
