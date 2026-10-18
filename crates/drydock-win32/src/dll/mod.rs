//! Rust-provided system DLLs.

pub mod advapi32;
pub mod dialog;
pub mod fonts;
pub mod gdi32;
pub mod gdi32_bitmap;
pub mod kernel32;
pub mod kernel32_file;
pub mod kernel32_misc;
pub mod kernel32_nls;
pub mod menu;
pub mod misc;
pub mod msacm32;
pub mod msvfw32;
pub mod user32;
pub mod user32_misc;
pub mod version;
pub mod winmm;

use crate::api::ExportDef;

pub struct BuiltinDll {
    /// Canonical upper-case file name.
    pub name: &'static str,
    pub parts: &'static [&'static [ExportDef]],
    /// Process-attach behaviour of the system DLL (e.g. registering window classes).
    pub on_load: Option<fn(&mut crate::session::Session)>,
}

macro_rules! dll {
    ($name:literal, $($part:expr),+) => {
        BuiltinDll { name: $name, parts: &[$($part),+], on_load: None }
    };
}

pub static BUILTINS: &[BuiltinDll] = &[
    dll!("DRYDOCK.DLL", kernel32::DRYDOCK_EXPORTS),
    dll!(
        "KERNEL32.DLL",
        kernel32::KERNEL32_EXPORTS,
        kernel32_misc::KERNEL32_MISC_EXPORTS
    ),
    dll!("NTDLL.DLL", misc::NTDLL_EXPORTS),
    dll!(
        "USER32.DLL",
        user32::USER32_EXPORTS,
        dialog::DIALOG_EXPORTS,
        user32_misc::USER32_MISC_EXPORTS,
        menu::MENU_EXPORTS
    ),
    dll!("GDI32.DLL", gdi32::GDI32_EXPORTS, gdi32_bitmap::GDI32_BITMAP_EXPORTS),
    dll!("ADVAPI32.DLL", advapi32::ADVAPI32_EXPORTS),
    dll!("WINMM.DLL", winmm::WINMM_EXPORTS),
    dll!("SHELL32.DLL", misc::SHELL32_EXPORTS),
    dll!("IMM32.DLL", misc::IMM32_EXPORTS),
    dll!("COMDLG32.DLL", misc::COMDLG32_EXPORTS),
    BuiltinDll {
        name: "RICHED32.DLL",
        parts: &[dialog::RICHED32_EXPORTS],
        on_load: Some(dialog::register_richedit),
    },
    BuiltinDll {
        name: "MSVFW32.DLL",
        parts: &[msvfw32::MSVFW32_EXPORTS],
        on_load: Some(msvfw32::on_load),
    },
    dll!("OLE32.DLL", crate::com::OLE32_EXPORTS),
    dll!("DDRAW.DLL", crate::com::DDRAW_EXPORTS),
    dll!("DINPUT.DLL", crate::com::DINPUT_EXPORTS),
    dll!("DSOUND.DLL", crate::com::DSOUND_EXPORTS),
    dll!(
        "DPLAYX.DLL",
        crate::com::DPLAYX_EXPORTS,
        crate::com::dplay::DPLAYX_LOBBY_EXPORTS
    ),
    dll!("DMUSIC.DLL", crate::com::dmusic::DMUSIC_EXPORTS),
    dll!("VERSION.DLL", version::VERSION_EXPORTS),
    dll!("AVIFIL32.DLL", &[]),
    dll!("MSACM32.DLL", msacm32::MSACM32_EXPORTS),
    dll!("AMSTREAM.DLL", &[]),
];
