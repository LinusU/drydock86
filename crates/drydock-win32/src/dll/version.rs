//! version.dll: file version resources (VS_VERSIONINFO).
//!
//! GetFileVersionInfoA returns the file's RT_VERSION resource (Unicode) followed by an
//! equally sized area where VerQueryValueA places ANSI copies of string values, at the
//! same offset as the Unicode value plus the resource size.

use crate::api::*;
use crate::export;
use crate::session::Session;
use drydock_core::pe::{PeImage, ResId};

/// The RT_VERSION resource of the file `name`, if it has one.
fn version_resource(s: &mut Session, name: &str) -> Option<Vec<u8>> {
    let data = crate::dll::msvfw32::read_guest_file(s, name).ok()?;
    let pe = PeImage::parse(&data).ok()?;
    let img = pe.map_image(&data);
    let r = pe.resources(&img).ok()?.into_iter().find(|r| r.ty == ResId::Id(16))?;
    img.get(r.data_rva as usize..(r.data_rva + r.size) as usize)
        .map(|b| b.to_vec())
}

fn get_file_version_info_size_a(s: &mut Session) -> ApiResult {
    let [name, handle] = s.args::<2>()?;
    let name = s.read_str(name)?;
    if handle != 0 {
        s.mem.write_u32(handle, 0)?;
    }
    match version_resource(s, &name) {
        Some(r) => Ok(ApiRet::Ret(r.len() as u32 * 2)),
        None => {
            s.set_last_error(1813); // ERROR_RESOURCE_TYPE_NOT_FOUND
            Ok(ApiRet::Ret(0))
        }
    }
}

fn get_file_version_info_a(s: &mut Session) -> ApiResult {
    let [name, _handle, len, buf] = s.args::<4>()?;
    let name = s.read_str(name)?;
    let Some(r) = version_resource(s, &name) else {
        s.set_last_error(1813);
        return Ok(ApiRet::Ret(0));
    };
    let mut b = r.clone();
    b.resize(r.len() * 2, 0);
    b.truncate(len as usize);
    s.mem.write(buf, &b)?;
    Ok(ApiRet::Ret(1))
}

/// A node of the version tree: (key, value offset, value length field, type, children).
struct Node {
    key: String,
    value: usize,
    value_len: usize,
    text: bool,
    end: usize,
    children: usize,
}

fn node_at(b: &[u8], o: usize) -> Option<Node> {
    let g = |k: usize| b.get(k..k + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as usize);
    let len = g(o)?;
    let value_len = g(o + 2)?;
    let text = g(o + 4)? == 1;
    let mut k = o + 6;
    let mut w = Vec::new();
    loop {
        let c = g(k)? as u16;
        k += 2;
        if c == 0 {
            break;
        }
        w.push(c);
    }
    let value = (k + 3) & !3;
    let vbytes = if text { value_len * 2 } else { value_len };
    let children = (value + vbytes + 3) & !3;
    Some(Node {
        key: String::from_utf16_lossy(&w),
        value,
        value_len,
        text,
        end: o + len.max(6),
        children,
    })
}

/// Find the node at `path` ("\" separated) below the root.
fn find(b: &[u8], path: &str) -> Option<Node> {
    let mut node = node_at(b, 0)?;
    for comp in path.split('\\').filter(|c| !c.is_empty()) {
        let mut o = node.children;
        let mut found = None;
        while o + 6 <= node.end.min(b.len()) {
            let child = node_at(b, o)?;
            let next = (child.end + 3) & !3;
            if child.key.eq_ignore_ascii_case(comp) {
                found = Some(child);
                break;
            }
            if next <= o {
                break;
            }
            o = next;
        }
        node = found?;
    }
    Some(node)
}

fn ver_query_value_a(s: &mut Session) -> ApiResult {
    let [block, sub, pp, plen] = s.args::<4>()?;
    let path = s.read_str(sub)?;
    // The resource length is the root node's wLength.
    let total = s.mem.read_u16(block)? as u32;
    let b = s.mem.read_vec(block, total)?;
    let Some(n) = find(&b, &path) else {
        return Ok(ApiRet::Ret(0));
    };
    if n.value_len == 0 {
        return Ok(ApiRet::Ret(0));
    }
    let (ptr, len) = if n.text {
        // ANSI copy in the second half of the block.
        let w: Vec<u16> = b[n.value..n.value + n.value_len * 2]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let mut a = crate::strings::utf16_to_ansi(&w);
        a.push(0);
        let at = block + total + n.value as u32;
        s.mem.write(at, &a)?;
        (at, n.value_len as u32)
    } else {
        (block + n.value as u32, n.value_len as u32)
    };
    s.mem.write_u32(pp, ptr)?;
    if plen != 0 {
        s.mem.write_u32(plen, len)?;
    }
    Ok(ApiRet::Ret(1))
}

pub static VERSION_EXPORTS: &[ExportDef] = &[
    export!("GetFileVersionInfoA", 4, get_file_version_info_a),
    export!("GetFileVersionInfoSizeA", 2, get_file_version_info_size_a),
    export!("VerQueryValueA", 4, ver_query_value_a),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0u8; 6];
        b.extend(key.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()));
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        b.extend_from_slice(value);
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        for c in children {
            b.extend_from_slice(c);
            while !b.len().is_multiple_of(4) {
                b.push(0);
            }
        }
        let vl = if text { value.len() / 2 } else { value.len() } as u16;
        let l = b.len() as u16;
        b[0..2].copy_from_slice(&l.to_le_bytes());
        b[2..4].copy_from_slice(&vl.to_le_bytes());
        b[4..6].copy_from_slice(&(text as u16).to_le_bytes());
        b
    }

    #[test]
    fn walks_the_tree() {
        let ver: Vec<u8> = "1.0".encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
        let s = node("FileVersion", &ver, true, &[]);
        let table = node("040904b0", &[], true, &[s]);
        let sfi = node("StringFileInfo", &[], true, &[table]);
        let root = node("VS_VERSION_INFO", &[0xbd, 0x04, 0xef, 0xfe], false, &[sfi]);
        assert_eq!(find(&root, "\\").unwrap().value_len, 4);
        let n = find(&root, "\\StringFileInfo\\040904B0\\FileVersion").unwrap();
        assert!(n.text);
        assert_eq!(n.value_len, 4);
        assert!(find(&root, "\\StringFileInfo\\040904b0\\Missing").is_none());
    }
}
