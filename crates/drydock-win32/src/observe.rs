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

    pub fn threads_info_static(&self) -> Vec<ThreadInfo> {
        self.sched
            .threads
            .values()
            .map(|t| ThreadInfo {
                tid: t.tid,
                state: match &t.state {
                    crate::sched::ThreadState::Ready => "ready".into(),
                    crate::sched::ThreadState::Resume { .. } => "resuming".into(),
                    crate::sched::ThreadState::Blocked { api, block } => {
                        format!(
                            "blocked in {} on {:?} until {:?}",
                            self.thunks.name_of(*api),
                            block.wait,
                            block.deadline
                        )
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
            crate::input::InputEvent::Key {
                vk,
                scan,
                extended,
                down: true,
            },
        );
        self.user.input.schedule(
            at + hold_ns,
            crate::input::InputEvent::Key {
                vk,
                scan,
                extended,
                down: false,
            },
        );
    }

    pub fn key(&mut self, vk: u8, down: bool, at: u64) {
        let scan = crate::input::vk_to_scan(vk);
        let extended = crate::input::vk_is_extended(vk);
        self.user.input.schedule(
            at,
            crate::input::InputEvent::Key {
                vk,
                scan,
                extended,
                down,
            },
        );
    }

    pub fn answer_dialog(&mut self, button: u32) -> bool {
        match self.user.dialogs.iter_mut().find(|d| d.answer.is_none()) {
            Some(d) => {
                d.answer = Some(button);
                let id = d.id;
                crate::dll::dialog::answer_message_box(self, id, button);
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

impl Session {
    /// RGBA8 copy of the scanned-out image for a presentation window. Unlike
    /// [`Session::capture_front`] this never alters guest memory or surface ownership.
    pub fn present_rgba(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let id = self.front_surface();
        let sf = self.display.surfaces.get(&id)?.clone();
        let (w, h, mut px) = if sf.gpu_dirty {
            let r = self.host.renderer.as_mut()?;
            r.readback(id)?
        } else {
            let pal = self.palette_of(id);
            let data = self.mem.slice(sf.mem, sf.pitch * sf.h).ok()?;
            let bpp = sf.fmt.bytes_pp() as usize;
            let mut out = Vec::with_capacity((sf.w * sf.h * 4) as usize);
            for y in 0..sf.h as usize {
                let row = &data[y * sf.pitch as usize..];
                for x in 0..sf.w as usize {
                    let p = sf.fmt.decode(&row[x * bpp..x * bpp + bpp], pal.as_ref());
                    out.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
                }
            }
            (sf.w, sf.h, out)
        };
        apply_gamma(&mut px, &self.com.ddraw.gamma);
        Some((w, h, px))
    }

    /// Identity and write version of the front surface; changes whenever the CPU
    /// writes to it (GDI drawing) or the front surface is replaced.
    pub fn present_stamp(&mut self) -> (u32, u64) {
        let id = self.front_surface();
        (id, self.display.surfaces.get(&id).map_or(0, |sf| sf.version))
    }

    /// Like [`Session::present_rgba`], but a GPU-resident image is read back
    /// asynchronously so the caller does not wait for the GPU.
    pub fn present_begin(&mut self) -> Option<PresentJob> {
        let id = self.front_surface();
        let sf = self.display.surfaces.get(&id)?;
        if sf.gpu_dirty {
            let gamma = self.com.ddraw.gamma.clone();
            let r = self.host.renderer.as_mut()?;
            return Some(PresentJob::Gpu(r.readback_async(id)?, gamma));
        }
        let (w, h, px) = self.present_rgba()?;
        Some(PresentJob::Ready(w, h, px))
    }

    /// Let asynchronous readbacks make progress.
    pub fn present_poll(&self) {
        if let Some(r) = self.host.renderer.as_ref() {
            r.poll();
        }
    }
}

/// A presentation image being produced (see [`Session::present_begin`]).
pub enum PresentJob {
    Ready(u32, u32, Vec<u8>),
    Gpu(drydock_gfx::PendingReadback, Vec<u16>),
}

impl PresentJob {
    pub fn try_finish(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        match self {
            PresentJob::Ready(w, h, px) => Some((*w, *h, std::mem::take(px))),
            PresentJob::Gpu(r, gamma) => {
                let (w, h, mut px) = r.try_take()?;
                apply_gamma(&mut px, gamma);
                Some((w, h, px))
            }
        }
    }
}

/// Gamma ramp of the primary surface (identity unless the guest set one).
fn apply_gamma(px: &mut [u8], g: &[u16]) {
    for p in px.as_chunks_mut::<4>().0 {
        if g.len() == 768 {
            p[0] = (g[p[0] as usize] >> 8) as u8;
            p[1] = (g[256 + p[1] as usize] >> 8) as u8;
            p[2] = (g[512 + p[2] as usize] >> 8) as u8;
        }
        p[3] = 255;
    }
}
