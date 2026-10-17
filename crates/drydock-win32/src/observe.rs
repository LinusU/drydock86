//! Observations exposed to controllers: screenshots, window lists, summaries.

use crate::session::Session;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub hwnd: u32,
    pub class: String,
    pub title: String,
    pub visible: bool,
    pub rect: [i32; 4],
    pub parent: u32,
    pub tid: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadInfo {
    pub tid: u32,
    pub state: String,
    pub eip: u32,
    pub esp: u32,
    pub priority: i32,
    pub frames: usize,
    pub internal: bool,
}

impl Session {
    /// RGBA8 image of the surface currently scanned out.
    pub fn capture_front(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let id = self.front_surface();
        self.capture_surface(id)
    }

    pub fn capture_surface(&mut self, id: u32) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h, px) = self.surface_rgba(id)?;
        let mut out = Vec::with_capacity(px.len() * 4);
        for p in px {
            out.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
        }
        Some((w, h, out))
    }

    pub fn windows_info(&self) -> Vec<WindowInfo> {
        self.user
            .windows
            .values()
            .map(|w| WindowInfo {
                hwnd: w.hwnd,
                class: w.class.clone(),
                title: w.title.clone(),
                visible: w.visible,
                rect: [w.rect.left, w.rect.top, w.rect.right, w.rect.bottom],
                parent: w.parent,
                tid: w.tid,
            })
            .collect()
    }

    pub fn threads_info(&mut self) -> Vec<ThreadInfo> {
        self.sync_regs();
        self.sched
            .threads
            .values()
            .map(|t| ThreadInfo {
                tid: t.tid,
                state: match &t.state {
                    crate::sched::ThreadState::Ready => "ready".into(),
                    crate::sched::ThreadState::Resume { .. } => "resuming".into(),
                    crate::sched::ThreadState::Blocked { api, block } => {
                        format!("blocked in {} on {:?}", self.thunks.name_of(*api), block.wait)
                    }
                    crate::sched::ThreadState::Terminated => format!("exited ({:?})", t.exit_code),
                },
                eip: t.regs.eip,
                esp: t.regs.esp,
                priority: t.priority,
                frames: t.frames.len(),
                internal: t.internal,
            })
            .collect()
    }

    /// Schedule a key press (down now, up after `hold_ns`).
    pub fn tap_key(&mut self, vk: u8, at: u64, hold_ns: u64) {
        let scan = crate::input::vk_to_scan(vk);
        let extended = crate::input::vk_is_extended(vk);
        self.user.input.schedule(
            at,
            crate::input::InputEvent::Key { vk, scan, extended, down: true },
        );
        self.user.input.schedule(
            at + hold_ns,
            crate::input::InputEvent::Key { vk, scan, extended, down: false },
        );
    }

    pub fn key(&mut self, vk: u8, down: bool, at: u64) {
        let scan = crate::input::vk_to_scan(vk);
        let extended = crate::input::vk_is_extended(vk);
        self.user
            .input
            .schedule(at, crate::input::InputEvent::Key { vk, scan, extended, down });
    }

    pub fn answer_dialog(&mut self, button: u32) -> bool {
        match self.user.dialogs.iter_mut().find(|d| d.answer.is_none()) {
            Some(d) => {
                d.answer = Some(button);
                true
            }
            None => false,
        }
    }
}

pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(rgba).unwrap();
    }
    out
}
