//! COM infrastructure and DirectX components.
//!
//! A COM interface pointer is runtime-owned guest memory `[vtbl][object id]`. Vtables are
//! arrays of method thunks allocated inside the implementing builtin DLL image, so method
//! calls cross into Rust like any other API. Objects live in [`ComState::objs`] with
//! stable ids and explicit reference counts.

pub mod amstream;
pub mod d3d;
pub mod d3d6;
pub mod ddraw;
pub mod dinput;
pub mod dinput_ff;
pub mod dmusic;
pub mod dplay;
pub mod dsound;
pub mod ole;

use crate::api::*;
use crate::session::Session;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Guid = [u8; 16];

pub const S_OK: u32 = 0;
pub const S_FALSE: u32 = 1;
pub const E_NOINTERFACE: u32 = 0x8000_4002;
pub const E_POINTER: u32 = 0x8000_4003;
pub const E_NOTIMPL: u32 = 0x8000_4001;
pub const E_INVALIDARG: u32 = 0x8007_0057;
pub const E_OUTOFMEMORY: u32 = 0x8007_000e;
pub const E_FAIL: u32 = 0x8000_4005;

/// Parse "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" into in-memory GUID layout.
pub const fn guid(s: &str) -> Guid {
    let b = s.as_bytes();
    const fn hex(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("bad guid"),
        }
    }
    const fn byte(b: &[u8], i: usize) -> u8 {
        hex(b[i]) << 4 | hex(b[i + 1])
    }
    // Data1 (LE u32), Data2 (LE u16), Data3 (LE u16), Data4 (8 bytes as written).
    [
        byte(b, 6),
        byte(b, 4),
        byte(b, 2),
        byte(b, 0),
        byte(b, 11),
        byte(b, 9),
        byte(b, 16),
        byte(b, 14),
        byte(b, 19),
        byte(b, 21),
        byte(b, 24),
        byte(b, 26),
        byte(b, 28),
        byte(b, 30),
        byte(b, 32),
        byte(b, 34),
    ]
}

pub fn guid_str(g: &Guid) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

pub const IID_IUNKNOWN: Guid = guid("00000000-0000-0000-c000-000000000046");

/// Static description of a COM interface implemented in Rust.
pub struct IfaceDef {
    pub name: &'static str,
    pub iid: Guid,
    /// Builtin DLL whose image holds the vtable thunks.
    pub module: &'static str,
    /// Methods in vtable order. Stdcall arg counts include `this`.
    pub methods: &'static [ExportDef],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ComKind {
    DirectDraw(ddraw::DDrawObj),
    Surface(ddraw::SurfObj),
    Palette(ddraw::PalObj),
    Clipper(ddraw::ClipperObj),
    Direct3D,
    Device(Box<d3d::Device>),
    VertexBuffer(d3d::VertexBuffer),
    DInput(dinput::DInputObj),
    DInputDevice(Box<dinput::DiDevice>),
    DInputEffect(Box<dinput_ff::DiEffect>),
    DSound(dsound::DSoundObj),
    DSBuffer(Box<dsound::DsBuffer>),
    Ds3dListener(u32),
    Ds3dBuffer(u32),
    ClassFactory(Guid),
    DMusic(dmusic::DmKind),
    DPlayLobby,
    Viewport3(Box<d3d6::Viewport3>),
    Material3(d3d6::Material3),
    Light3(Box<d3d6::Light3>),
    MmStream(Box<amstream::MmStream>),
    MediaStream(amstream::MediaStream),
    StreamSample(amstream::StreamSample),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComObj {
    pub id: u32,
    pub refs: u32,
    pub kind: ComKind,
    /// Interface name → guest interface pointer.
    pub ifaces: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComState {
    pub objs: BTreeMap<u32, ComObj>,
    pub next_id: u32,
    /// Interface name → guest vtable address.
    pub vtables: BTreeMap<String, u32>,
    /// Guest interface pointer → (object id, interface name).
    pub ptrs: BTreeMap<u32, (u32, String)>,
    pub initialized: u32,
    /// GUIDs made by CoCreateGuid (they are derived from this count).
    #[serde(default)]
    pub guids_made: u32,
    pub ddraw: ddraw::DDrawGlobal,
    pub dsound: dsound::DSoundGlobal,
    pub dmusic: dmusic::DMusicGlobal,
}

impl Session {
    pub fn com_new(&mut self, kind: ComKind) -> u32 {
        let id = self.com.next_id.max(1);
        self.com.next_id = id + 1;
        self.com.objs.insert(
            id,
            ComObj {
                id,
                refs: 1,
                kind,
                ifaces: BTreeMap::new(),
            },
        );
        id
    }

    pub fn com_obj(&self, id: u32) -> Option<&ComObj> {
        self.com.objs.get(&id)
    }
    pub fn com_obj_mut(&mut self, id: u32) -> Option<&mut ComObj> {
        self.com.objs.get_mut(&id)
    }

    fn vtable(&mut self, def: &'static IfaceDef) -> Result<u32, ApiError> {
        if let Some(v) = self.com.vtables.get(def.name) {
            return Ok(*v);
        }
        let n = def.methods.len() as u32;
        let vt = self.sys_alloc(n * 4)?;
        for (i, m) in def.methods.iter().enumerate() {
            let name = format!("{}::{}", def.name, m.name);
            let t =
                crate::loader::internal_thunk(self, def.module, &name, m.conv, m.handler).map_err(ApiError::Fatal)?;
            self.mem.write_u32(vt + 4 * i as u32, t)?;
        }
        self.com.vtables.insert(def.name.to_string(), vt);
        Ok(vt)
    }

    /// Interface pointer for `obj` as `def`, creating it on first use.
    pub fn iface(&mut self, obj: u32, def: &'static IfaceDef) -> Result<u32, ApiError> {
        if let Some(p) = self.com_obj(obj).and_then(|o| o.ifaces.get(def.name)) {
            return Ok(*p);
        }
        let vt = self.vtable(def)?;
        let p = self.sys_alloc(16)?;
        self.mem.write_u32(p, vt)?;
        self.mem.write_u32(p + 4, obj)?;
        self.com.ptrs.insert(p, (obj, def.name.to_string()));
        if let Some(o) = self.com_obj_mut(obj) {
            o.ifaces.insert(def.name.to_string(), p);
        }
        Ok(p)
    }

    /// Resolve an interface pointer to its object id.
    pub fn obj_of(&self, ptr: u32) -> Option<u32> {
        self.com
            .ptrs
            .get(&ptr)
            .map(|(o, _)| *o)
            .filter(|o| self.com.objs.contains_key(o))
    }

    /// The object behind `this` (argument 0) of a COM method call.
    pub fn this_obj(&self) -> Result<u32, ApiError> {
        let this = self.arg(0)?;
        self.obj_of(this)
            .ok_or_else(|| ApiError::Fatal(format!("COM method called on unknown interface pointer {this:#x}")))
    }

    pub fn com_addref(&mut self, id: u32) -> u32 {
        match self.com_obj_mut(id) {
            Some(o) => {
                o.refs += 1;
                o.refs
            }
            None => 0,
        }
    }

    pub fn com_release(&mut self, id: u32) -> u32 {
        let Some(o) = self.com_obj_mut(id) else {
            return 0;
        };
        o.refs = o.refs.saturating_sub(1);
        let r = o.refs;
        if r == 0 {
            self.com_destroy(id);
        }
        r
    }

    fn com_destroy(&mut self, id: u32) {
        let Some(o) = self.com.objs.remove(&id) else {
            return;
        };
        for p in o.ifaces.values() {
            self.com.ptrs.remove(p);
        }
        let sh = self.process.sys_heap;
        for p in o.ifaces.values() {
            self.heaps.free(sh, *p);
        }
        match o.kind {
            ComKind::Surface(sf) => ddraw::surface_destroyed(self, id, sf),
            ComKind::DSBuffer(b) => dsound::buffer_destroyed(self, id, *b),
            ComKind::DInputDevice(d) => dinput::device_destroyed(self, id, *d),
            ComKind::DInputEffect(e) => dinput_ff::effect_destroyed(self, id, *e),
            ComKind::Device(d) => d3d::device_destroyed(self, id, *d),
            ComKind::DirectDraw(d) => ddraw::ddraw_destroyed(self, id, d),
            ComKind::MmStream(m) => amstream::stream_destroyed(self, &m),
            ComKind::MediaStream(m) => amstream::media_stream_destroyed(self, &m),
            ComKind::StreamSample(x) => amstream::sample_destroyed(self, &x),
            _ => {}
        }
    }

    /// Write an interface pointer to an out parameter.
    pub fn out_iface(&mut self, pp: u32, obj: u32, def: &'static IfaceDef) -> Result<(), ApiError> {
        let p = self.iface(obj, def)?;
        self.mem.write_u32(pp, p)?;
        Ok(())
    }

    pub fn read_guid(&self, p: u32) -> Result<Guid, ApiError> {
        let v = self.mem.read_vec(p, 16)?;
        Ok(v.try_into().unwrap())
    }
}

/// Every interface definition, for re-binding vtable thunks after a restore.
pub static ALL_IFACES: &[&IfaceDef] = &[
    &dinput::IDIRECTINPUTA,
    &dinput::IDIRECTINPUT2A,
    &dinput::IDIRECTINPUT7A,
    &dinput::IDIRECTINPUTDEVICEA,
    &dinput::IDIRECTINPUTDEVICE2A,
    &dinput::IDIRECTINPUTDEVICE7A,
    &dinput_ff::IDIRECTINPUTEFFECT,
    &dsound::IDIRECTSOUND,
    &dsound::IDIRECTSOUNDBUFFER,
    &dsound::IDIRECTSOUNDNOTIFY,
    &dsound::IDIRECTSOUND3DLISTENER,
    &dsound::IDIRECTSOUND3DBUFFER,
    &ole::ICLASSFACTORY,
    &dplay::IDIRECTPLAYLOBBYA,
    &dplay::IDIRECTPLAYLOBBY2A,
    &dplay::IDIRECTPLAYLOBBY3A,
    &dmusic::IDIRECTMUSIC,
    &dmusic::IDIRECTMUSIC2,
    &dmusic::IDIRECTMUSICPORT,
    &dmusic::IDIRECTMUSICPERFORMANCE,
    &dmusic::IDIRECTMUSICPERFORMANCE2,
    &dmusic::IDIRECTMUSICLOADER,
    &dmusic::IDIRECTMUSICCOMPOSER,
    &d3d::IDIRECT3D7,
    &d3d::IDIRECT3DDEVICE7,
    &d3d::IDIRECT3DVERTEXBUFFER7,
    &d3d6::IDIRECT3D3,
    &d3d6::IDIRECT3DDEVICE3,
    &d3d6::IDIRECT3DVIEWPORT3,
    &d3d6::IDIRECT3DMATERIAL3,
    &d3d6::IDIRECT3DLIGHT,
    &d3d6::IDIRECT3DTEXTURE2,
    &d3d6::IDIRECT3DVERTEXBUFFER,
    &amstream::IAMMULTIMEDIASTREAM,
    &amstream::IMULTIMEDIASTREAM,
    &amstream::IDIRECTDRAWMEDIASTREAM,
    &amstream::IMEDIASTREAM,
    &amstream::IDIRECTDRAWSTREAMSAMPLE,
    &ddraw::IDIRECTDRAW7,
    &ddraw::IDIRECTDRAW4,
    &ddraw::IDIRECTDRAW2,
    &ddraw::IDIRECTDRAW,
    &ddraw::IDIRECTDRAWSURFACE7,
    &ddraw::IDIRECTDRAWSURFACE4,
    &ddraw::IDIRECTDRAWSURFACE3,
    &ddraw::IDIRECTDRAWSURFACE2,
    &ddraw::IDIRECTDRAWSURFACE,
    &ddraw::IDIRECTDRAWPALETTE,
    &ddraw::IDIRECTDRAWCLIPPER,
    &ddraw::IDIRECTDRAWGAMMACONTROL,
];

/// Interfaces each object kind answers to QueryInterface with.
fn supported(kind: &ComKind) -> Vec<&'static IfaceDef> {
    match kind {
        ComKind::DirectDraw(_) => ddraw::DDRAW_IFACES.to_vec(),
        ComKind::Surface(sf) => {
            let mut v = ddraw::SURFACE_IFACES.to_vec();
            if sf.caps & ddraw::DDSCAPS_TEXTURE != 0 {
                v.extend(d3d::TEXTURE_IFACES);
            }
            v
        }
        ComKind::Palette(_) => vec![&ddraw::IDIRECTDRAWPALETTE],
        ComKind::Clipper(_) => vec![&ddraw::IDIRECTDRAWCLIPPER],
        ComKind::Direct3D => vec![&d3d::IDIRECT3D7],
        ComKind::Device(d) => {
            if d.v3.is_some() {
                vec![&d3d6::IDIRECT3DDEVICE3]
            } else {
                vec![&d3d::IDIRECT3DDEVICE7]
            }
        }
        ComKind::VertexBuffer(_) => vec![&d3d::IDIRECT3DVERTEXBUFFER7, &d3d6::IDIRECT3DVERTEXBUFFER],
        ComKind::Viewport3(_) => vec![&d3d6::IDIRECT3DVIEWPORT3],
        ComKind::Material3(_) => vec![&d3d6::IDIRECT3DMATERIAL3],
        ComKind::Light3(_) => vec![&d3d6::IDIRECT3DLIGHT],
        ComKind::MmStream(_) | ComKind::MediaStream(_) | ComKind::StreamSample(_) => amstream::supported(kind),
        ComKind::DInput(_) => dinput::DINPUT_IFACES.to_vec(),
        ComKind::DInputDevice(_) => dinput::DEVICE_IFACES.to_vec(),
        ComKind::DInputEffect(_) => vec![&dinput_ff::IDIRECTINPUTEFFECT],
        ComKind::DSound(_) => vec![&dsound::IDIRECTSOUND],
        ComKind::DSBuffer(_) => dsound::BUFFER_IFACES.to_vec(),
        ComKind::Ds3dListener(_) => vec![&dsound::IDIRECTSOUND3DLISTENER],
        ComKind::Ds3dBuffer(_) => vec![&dsound::IDIRECTSOUND3DBUFFER],
        ComKind::ClassFactory(_) => vec![&ole::ICLASSFACTORY],
        ComKind::DMusic(k) => dmusic::supported(k),
        ComKind::DPlayLobby => dplay::LOBBY_IFACES.to_vec(),
    }
}

/// IUnknown::QueryInterface for every Rust-implemented object.
pub fn query_interface(s: &mut Session) -> ApiResult {
    let [_this, riid, ppv] = s.args::<3>()?;
    let id = s.this_obj()?;
    let iid = s.read_guid(riid)?;
    if ppv == 0 {
        return Ok(ApiRet::Ret(E_POINTER));
    }
    let kind = s.com_obj(id).unwrap().kind.clone();
    // Some interfaces are implemented by a different object (e.g. IDirect3D7 by the
    // DirectDraw object) — delegate those first.
    if let Some(r) = special_qi(s, id, &kind, &iid, ppv)? {
        return Ok(ApiRet::Ret(r));
    }
    let defs = supported(&kind);
    let def = if iid == IID_IUNKNOWN {
        defs.first().copied()
    } else {
        defs.iter().copied().find(|d| d.iid == iid)
    };
    match def {
        Some(d) => {
            s.out_iface(ppv, id, d)?;
            s.com_addref(id);
            Ok(ApiRet::Ret(S_OK))
        }
        None => {
            s.event(
                crate::diag::Severity::Info,
                "QueryInterface",
                format!(
                    "{:?} does not implement {}",
                    std::mem::discriminant(&kind),
                    guid_str(&iid)
                ),
            );
            s.mem.write_u32(ppv, 0)?;
            Ok(ApiRet::Ret(E_NOINTERFACE))
        }
    }
}

fn special_qi(s: &mut Session, id: u32, kind: &ComKind, iid: &Guid, ppv: u32) -> Result<Option<u32>, ApiError> {
    match kind {
        ComKind::DirectDraw(_) => d3d::ddraw_qi(s, id, iid, ppv),
        ComKind::DSBuffer(_) => dsound::buffer_qi(s, id, iid, ppv),
        ComKind::Surface(_) => ddraw::surface_qi(s, id, iid, ppv),
        _ => Ok(None),
    }
}

pub fn add_ref(s: &mut Session) -> ApiResult {
    let id = s.this_obj()?;
    Ok(ApiRet::Ret(s.com_addref(id)))
}

pub fn release(s: &mut Session) -> ApiResult {
    let id = s.this_obj()?;
    Ok(ApiRet::Ret(s.com_release(id)))
}

/// Shared vtable prefix.
#[macro_export]
macro_rules! iunknown {
    () => {
        [
            $crate::export!("QueryInterface", 3, $crate::com::query_interface),
            $crate::export!("AddRef", 1, $crate::com::add_ref),
            $crate::export!("Release", 1, $crate::com::release),
        ]
    };
}

/// A COM method that the emulator does not implement (halts with a diagnostic).
#[macro_export]
macro_rules! unimpl_method {
    ($name:literal, $n:literal) => {
        $crate::api::ExportDef {
            name: $name,
            ordinal: None,
            conv: $crate::api::Conv::Stdcall($n),
            handler: None,
        }
    };
}

// ---------------------------------------------------------------- enumeration

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ComCont {
    /// Call `cb` once per item (args per item), stopping when it returns `stop`.
    Enum {
        cb: u32,
        items: Vec<Vec<CallArg>>,
        idx: usize,
        stop: u32,
        ret: u32,
    },
    Ddraw(ddraw::DdCont),
    Dsound(dsound::DsCont),
    Dinput(dinput::DiCont),
}

pub fn enum_callbacks(cb: u32, items: Vec<Vec<CallArg>>, stop: u32, ret: u32) -> ApiResult {
    next_enum(cb, items, 0, stop, ret)
}

fn next_enum(cb: u32, items: Vec<Vec<CallArg>>, idx: usize, stop: u32, ret: u32) -> ApiResult {
    if idx >= items.len() {
        return Ok(ApiRet::Ret(ret));
    }
    let args = items[idx].clone();
    Ok(ApiRet::call(
        cb,
        args,
        Cont::Com(ComCont::Enum {
            cb,
            items,
            idx: idx + 1,
            stop,
            ret,
        }),
    ))
}

pub fn resume(s: &mut Session, c: ComCont, v: Resume) -> ApiResult {
    match c {
        ComCont::Enum {
            cb,
            items,
            idx,
            stop,
            ret,
        } => {
            if v.eax() == stop {
                return Ok(ApiRet::Ret(ret));
            }
            next_enum(cb, items, idx, stop, ret)
        }
        ComCont::Ddraw(c) => ddraw::resume(s, c, v),
        ComCont::Dsound(c) => dsound::resume(s, c, v),
        ComCont::Dinput(c) => dinput::resume(s, c, v),
    }
}

pub fn next_event(s: &Session) -> Option<u64> {
    let a = ddraw::next_event(s);
    let b = dsound::next_event(s);
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

pub fn service(s: &mut Session) {
    ddraw::service(s);
    dsound::service(s);
}

pub use ddraw::DDRAW_EXPORTS;
pub use dinput::DINPUT_EXPORTS;
pub use dsound::DSOUND_EXPORTS;
pub use ole::{DPLAYX_EXPORTS, OLE32_EXPORTS};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guid_layout() {
        let g = guid("6c14db80-a733-11ce-a521-0020af0be560");
        assert_eq!(&g[..4], &[0x80, 0xdb, 0x14, 0x6c]);
        assert_eq!(&g[4..6], &[0x33, 0xa7]);
        assert_eq!(&g[8..], &[0xa5, 0x21, 0x00, 0x20, 0xaf, 0x0b, 0xe5, 0x60]);
        assert_eq!(guid_str(&g), "6c14db80-a733-11ce-a521-0020af0be560");
    }
}
