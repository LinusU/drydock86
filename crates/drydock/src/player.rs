//! Interactive player: a desktop window over the same session runtime.
//!
//! The session runs on its own thread with realtime pacing. The window thread forwards
//! host keyboard/mouse input into the virtual input devices and displays the frames the
//! emulated display presents. Closing the window stops the session.

use crate::driver::{Driver, Pacing};
use crate::launch::RunArgs;
use anyhow::Result;
use drydock_win32::input::{InputEvent, vk_is_extended, vk_to_scan};
use drydock_win32::session::RunStop;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::Duration;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

enum ToSession {
    Input(InputEvent),
    /// Pointer position in guest screen coordinates.
    Pointer(i32, i32),
    Quit,
}

enum ToUi {
    Frame(Frame),
    Stopped(String),
}

/// Map a physical key to (virtual key, set-1 scan code, extended).
pub fn map_key(k: KeyCode) -> Option<u8> {
    use KeyCode::*;
    Some(match k {
        KeyA => b'A',
        KeyB => b'B',
        KeyC => b'C',
        KeyD => b'D',
        KeyE => b'E',
        KeyF => b'F',
        KeyG => b'G',
        KeyH => b'H',
        KeyI => b'I',
        KeyJ => b'J',
        KeyK => b'K',
        KeyL => b'L',
        KeyM => b'M',
        KeyN => b'N',
        KeyO => b'O',
        KeyP => b'P',
        KeyQ => b'Q',
        KeyR => b'R',
        KeyS => b'S',
        KeyT => b'T',
        KeyU => b'U',
        KeyV => b'V',
        KeyW => b'W',
        KeyX => b'X',
        KeyY => b'Y',
        KeyZ => b'Z',
        Digit0 => b'0',
        Digit1 => b'1',
        Digit2 => b'2',
        Digit3 => b'3',
        Digit4 => b'4',
        Digit5 => b'5',
        Digit6 => b'6',
        Digit7 => b'7',
        Digit8 => b'8',
        Digit9 => b'9',
        Escape => 0x1b,
        Enter => 0x0d,
        NumpadEnter => 0x0d,
        Space => 0x20,
        Tab => 0x09,
        Backspace => 0x08,
        ShiftLeft => 0xa0,
        ShiftRight => 0xa1,
        ControlLeft => 0xa2,
        ControlRight => 0xa3,
        AltLeft => 0xa4,
        AltRight => 0xa5,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        PageUp => 0x21,
        PageDown => 0x22,
        End => 0x23,
        Home => 0x24,
        Insert => 0x2d,
        Delete => 0x2e,
        F1 => 0x70,
        F2 => 0x71,
        F3 => 0x72,
        F4 => 0x73,
        F5 => 0x74,
        F6 => 0x75,
        F7 => 0x76,
        F8 => 0x77,
        F9 => 0x78,
        F10 => 0x79,
        F11 => 0x7a,
        F12 => 0x7b,
        Minus => 0xbd,
        Equal => 0xbb,
        BracketLeft => 0xdb,
        BracketRight => 0xdd,
        Backslash => 0xdc,
        Semicolon => 0xba,
        Quote => 0xde,
        Comma => 0xbc,
        Period => 0xbe,
        Slash => 0xbf,
        Backquote => 0xc0,
        CapsLock => 0x14,
        Numpad0 => 0x60,
        Numpad1 => 0x61,
        Numpad2 => 0x62,
        Numpad3 => 0x63,
        Numpad4 => 0x64,
        Numpad5 => 0x65,
        Numpad6 => 0x66,
        Numpad7 => 0x67,
        Numpad8 => 0x68,
        Numpad9 => 0x69,
        NumpadAdd => 0x6b,
        NumpadSubtract => 0x6d,
        NumpadMultiply => 0x6a,
        NumpadDivide => 0x6f,
        NumpadDecimal => 0x6e,
        _ => return None,
    })
}

fn key_event(vk: u8, down: bool) -> InputEvent {
    let extended = vk_is_extended(vk);
    InputEvent::Key {
        vk,
        scan: vk_to_scan(vk),
        extended,
        down,
    }
}

fn session_thread(args: RunArgs, rx: Receiver<ToSession>, tx: SyncSender<ToUi>, proxy: EventLoopProxy<()>) {
    let mut s = match crate::launch::build_session(&args) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(ToUi::Stopped(format!("{e:#}")));
            let _ = proxy.send_event(());
            return;
        }
    };
    if let Err(e) = crate::launch::configure_host(&mut s, &args) {
        let _ = tx.send(ToUi::Stopped(format!("{e:#}")));
        let _ = proxy.send_event(());
        return;
    }
    s.user.auto_dismiss_dialogs = args.auto_dismiss_dialogs;
    if let Some(p) = &args.replay_input
        && let Err(e) = crate::launch::schedule_replay(&mut s, p)
    {
        let _ = tx.send(ToUi::Stopped(format!("{e:#}")));
        let _ = proxy.send_event(());
        return;
    }
    let mut d = Driver::new(s, Pacing::Realtime);
    let mut last_frame = u64::MAX;
    let mut last_rel: Option<(i32, i32)> = None;
    loop {
        loop {
            match rx.try_recv() {
                Ok(ToSession::Input(ev)) => {
                    let now = d.s.clock.now_ns();
                    d.s.user.input.schedule(now, ev);
                }
                Ok(ToSession::Pointer(x, y)) => {
                    let now = d.s.clock.now_ns();
                    d.s.user.input.schedule(now, InputEvent::MouseMove { x, y });
                    let _ = last_rel;
                    last_rel = Some((x, y));
                }
                Ok(ToSession::Quit) | Err(TryRecvError::Disconnected) => {
                    let now = d.s.clock.now_ns();
                    d.s.user.input.release_all(now);
                    let _ = d.s.vfs.persist();
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        let stop = d.advance(4_000_000, false);
        if args.seconds > 0.0 && d.s.clock.now_ns() as f64 / 1e9 >= args.seconds && args.play_limit {
            let rep = crate::launch::report(&d, &stop);
            let dir = args.artifacts.clone().unwrap_or_else(|| "local/artifacts/play".into());
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&rep).unwrap_or_default());
            if let Some((w, h, px)) = d.s.present_rgba() {
                let _ = std::fs::write(dir.join("final.png"), drydock_win32::observe::encode_png(w, h, &px));
            }
            let _ = tx.send(ToUi::Stopped("time limit reached".into()));
            let _ = proxy.send_event(());
            return;
        }
        if d.s.frames_presented != last_frame {
            last_frame = d.s.frames_presented;
            if let Some((w, h, rgba)) = d.s.present_rgba() {
                let _ = tx.try_send(ToUi::Frame(Frame { w, h, rgba }));
                let _ = proxy.send_event(());
            }
        }
        match stop {
            RunStop::TimeReached | RunStop::Presented { .. } => {}
            RunStop::Idle { .. } => std::thread::sleep(Duration::from_millis(1)),
            other => {
                let detail = match &d.s.diag.halt {
                    Some(h) => format!("{other:?}: {} {}", h.reason, h.detail),
                    None => format!("{other:?}"),
                };
                let rep = crate::launch::report(&d, &other);
                let dir = args.artifacts.clone().unwrap_or_else(|| "local/artifacts/play".into());
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&rep).unwrap_or_default());
                let _ = tx.send(ToUi::Stopped(detail));
                let _ = proxy.send_event(());
                return;
            }
        }
    }
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    tex: Option<(wgpu::Texture, wgpu::BindGroup, u32, u32)>,
}

const BLIT: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
struct V { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32> };
struct R { scale: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    var pos = array<vec2<f32>, 3>(vec2(-1.0, -3.0), vec2(-1.0, 1.0), vec2(3.0, 1.0));
    var o: V;
    o.p = vec4(pos[i], 0.0, 1.0);
    o.uv = vec2((pos[i].x + 1.0) * 0.5, (1.0 - pos[i].y) * 0.5);
    return o;
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> { return textureSample(t, s, v.uv); }
"#;

impl Gfx {
    fn new(window: Arc<Window>) -> Result<Gfx, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).map_err(|e| e.to_string())?;
        let size = window.inner_size();
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface not supported")?;
        surface.configure(&device, &config);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(config.format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Gfx {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            layout,
            sampler,
            tex: None,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    fn upload(&mut self, f: &Frame) {
        let need = !matches!(&self.tex, Some((_, _, w, h)) if *w == f.w && *h == f.h);
        if need {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("frame"),
                size: wgpu::Extent3d {
                    width: f.w,
                    height: f.h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.tex = Some((tex, bg, f.w, f.h));
        }
        let (tex, _, _, _) = self.tex.as_ref().unwrap();
        self.queue.write_texture(
            tex.as_image_copy(),
            &f.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(f.w * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: f.w,
                height: f.h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Letterboxed viewport of the guest image inside the window.
    fn viewport(&self) -> (f32, f32, f32, f32) {
        let (gw, gh) = self.tex.as_ref().map(|t| (t.2 as f32, t.3 as f32)).unwrap_or((640.0, 480.0));
        let (ww, wh) = (self.config.width as f32, self.config.height as f32);
        let scale = (ww / gw).min(wh / gh);
        let (vw, vh) = (gw * scale, gh * scale);
        ((ww - vw) / 2.0, (wh - vh) / 2.0, vw, vh)
    }

    fn draw(&mut self) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some((_, bg, _, _)) = &self.tex {
                let (x, y, w, h) = self.viewport();
                rp.set_viewport(x, y, w.max(1.0), h.max(1.0), 0.0, 1.0);
                rp.set_pipeline(&self.pipeline);
                rp.set_bind_group(0, bg, &[]);
                rp.draw(0..3, 0..1);
            }
        }
        self.queue.submit([enc.finish()]);
        frame.present();
    }
}

struct App {
    args: RunArgs,
    gfx: Option<Gfx>,
    to_session: Option<SyncSender<ToSession>>,
    from_session: Option<Receiver<ToUi>>,
    proxy: EventLoopProxy<()>,
    status: Option<String>,
    guest_size: (u32, u32),
}

impl App {
    fn send(&self, m: ToSession) {
        if let Some(tx) = &self.to_session {
            let _ = tx.send(m);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Drydock")
            .with_inner_size(winit::dpi::LogicalSize::new(1024.0, 768.0));
        let window = Arc::new(el.create_window(attrs).expect("window"));
        match Gfx::new(window.clone()) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => {
                eprintln!("presentation unavailable: {e}");
                el.exit();
                return;
            }
        }
        let (tx, rx) = sync_channel::<ToSession>(1024);
        let (utx, urx) = sync_channel::<ToUi>(2);
        let args = self.args.clone();
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("drydock-session".into())
            .stack_size(64 << 20)
            .spawn(move || session_thread(args, rx, utx, proxy))
            .expect("session thread");
        self.to_session = Some(tx);
        self.from_session = Some(urx);
        // The emulated application starts with focus.
        self.send(ToSession::Input(InputEvent::Focus { active: true }));
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _e: ()) {
        let Some(rx) = &self.from_session else { return };
        let mut latest = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                ToUi::Frame(f) => latest = Some(f),
                ToUi::Stopped(why) => {
                    eprintln!("session stopped: {why}");
                    self.status = Some(why);
                    el.exit();
                }
            }
        }
        if let (Some(f), Some(g)) = (latest, self.gfx.as_mut()) {
            self.guest_size = (f.w, f.h);
            g.upload(&f);
            g.window.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => {
                self.send(ToSession::Quit);
                el.exit();
            }
            WindowEvent::Resized(sz) => {
                if let Some(g) = self.gfx.as_mut() {
                    g.resize(sz.width, sz.height);
                    g.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(g) = self.gfx.as_mut() {
                    g.draw();
                }
            }
            WindowEvent::Focused(f) => self.send(ToSession::Input(InputEvent::Focus { active: f })),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.repeat {
                    return;
                }
                if let PhysicalKey::Code(c) = event.physical_key
                    && let Some(vk) = map_key(c)
                {
                    self.send(ToSession::Input(key_event(vk, event.state == ElementState::Pressed)));
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(g) = &self.gfx {
                    let (vx, vy, vw, vh) = g.viewport();
                    let (gw, gh) = (self.guest_size.0.max(1) as f32, self.guest_size.1.max(1) as f32);
                    let x = ((position.x as f32 - vx) / vw * gw).clamp(0.0, gw - 1.0) as i32;
                    let y = ((position.y as f32 - vy) / vh * gh).clamp(0.0, gh - 1.0) as i32;
                    self.send(ToSession::Pointer(x, y));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    _ => return,
                };
                self.send(ToSession::Input(InputEvent::MouseButton {
                    button: b,
                    down: state == ElementState::Pressed,
                }));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 120.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => p.y as i32,
                };
                self.send(ToSession::Input(InputEvent::Wheel { delta: d }));
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: winit::event::DeviceId, ev: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = ev {
            let _ = delta;
        }
    }
}

pub fn run(args: RunArgs) -> Result<()> {
    let el = EventLoop::new()?;
    el.set_control_flow(ControlFlow::Wait);
    let proxy = el.create_proxy();
    let mut app = App {
        args,
        gfx: None,
        to_session: None,
        from_session: None,
        proxy,
        status: None,
        guest_size: (640, 480),
    };
    el.run_app(&mut app)?;
    if let Some(s) = app.status {
        eprintln!("{s}");
    }
    Ok(())
}
