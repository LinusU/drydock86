//! TrueType fonts installed in the guest's Fonts folder (`<windows dir>\Fonts`).
//!
//! GDI uses an installed font when a LOGFONT names its family or full name, with the
//! font's own glyphs and Windows metrics (usWinAscent/usWinDescent); other faces are
//! drawn with the bundled Liberation fonts. The folder is read once per session, on
//! the first text operation. Parsed fonts are shared host-side by content.

use crate::session::Session;
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// One installed font face.
#[derive(Clone)]
pub struct GuestFont {
    /// Family and full names in every language the font names them in, upper case
    /// (US English first).
    pub families: Vec<String>,
    pub fulls: Vec<String>,
    pub weight: u16,
    pub italic: bool,
    pub font: &'static fontdue::Font,
    /// usWinAscent and usWinDescent per em.
    pub ascent: f32,
    pub descent: f32,
}

/// Parsed font files by content hash, kept for the life of the process.
fn parsed(data: &[u8]) -> Option<&'static fontdue::Font> {
    static CACHE: OnceLock<Mutex<HashMap<[u8; 32], Option<&'static fontdue::Font>>>> = OnceLock::new();
    let key: [u8; 32] = sha2::Sha256::digest(data).into();
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap();
    *c.entry(key).or_insert_with(|| {
        fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
            .ok()
            .map(|f| &*Box::leak(Box::new(f)))
    })
}

fn describe(data: &[u8]) -> Option<GuestFont> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let names = |id: u16| {
        let mut v: Vec<(bool, String)> = face
            .names()
            .into_iter()
            .filter(|n| n.name_id == id && n.is_unicode())
            .filter_map(|n| Some((n.language_id != 0x409, n.to_string()?.to_uppercase())))
            .collect();
        v.sort_by_key(|x| x.0);
        let mut out: Vec<String> = Vec::new();
        for (_, n) in v {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    };
    let families = names(ttf_parser::name_id::FAMILY);
    if families.is_empty() {
        return None;
    }
    let mut fulls = names(ttf_parser::name_id::FULL_NAME);
    if fulls.is_empty() {
        fulls = families.clone();
    }
    let upem = face.units_per_em().max(1) as f32;
    let (asc, desc) = match face.tables().os2 {
        Some(os2) => (os2.windows_ascender() as f32, os2.windows_descender() as f32),
        None => (face.ascender() as f32, -(face.descender() as f32)),
    };
    Some(GuestFont {
        families,
        fulls,
        weight: face.weight().to_number(),
        italic: face.is_italic(),
        font: parsed(data)?,
        ascent: asc / upem,
        descent: desc.abs() / upem,
    })
}

impl Session {
    /// The fonts in the guest's Fonts folder.
    pub fn guest_fonts(&mut self) -> &[GuestFont] {
        if self.host.guest_fonts.is_none() {
            let mut list = Vec::new();
            let dir = format!("{}\\Fonts", self.config.windows.windows_dir);
            if let Some(gp) = self.guest_path(&dir) {
                for e in self.vfs.list_dir(&gp) {
                    let lower = e.name.to_lowercase();
                    if !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
                        continue;
                    }
                    let Ok(data) = crate::dll::msvfw32::read_guest_file(self, &format!("{dir}\\{}", e.name)) else {
                        continue;
                    };
                    match describe(&data) {
                        Some(f) => list.push(f),
                        None => self.event(
                            crate::diag::Severity::Warning,
                            "fonts",
                            format!("{dir}\\{} is not a usable TrueType font", e.name),
                        ),
                    }
                }
            }
            // Deterministic preference order regardless of directory listing order.
            list.sort_by(|a, b| (&a.families[0], a.weight, a.italic).cmp(&(&b.families[0], b.weight, b.italic)));
            self.host.guest_fonts = Some(list);
        }
        self.host.guest_fonts.as_deref().unwrap()
    }

    /// The installed font for a LOGFONT face name, weight and slant, as the GDI font
    /// mapper picks it: an exact full-name match, else the closest style of the family.
    pub fn find_guest_font(&mut self, face: &str, weight: i32, italic: bool) -> Option<GuestFont> {
        let face = face.to_uppercase();
        if face.is_empty() {
            return None;
        }
        let fonts = self.guest_fonts();
        if let Some(f) = fonts.iter().find(|f| f.fulls.contains(&face)) {
            return Some(f.clone());
        }
        let want = if weight <= 0 { 400 } else { weight };
        fonts
            .iter()
            .filter(|f| f.families.contains(&face))
            .min_by_key(|f| (f.weight as i32 - want).abs() + if f.italic != italic { 1000 } else { 0 })
            .cloned()
    }
}
