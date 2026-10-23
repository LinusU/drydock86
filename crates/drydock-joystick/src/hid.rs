//! HID report descriptors: the fields of each report and how to read and write them.

use std::collections::BTreeMap;

pub const PAGE_GENERIC_DESKTOP: u32 = 0x01;
pub const PAGE_BUTTON: u32 = 0x09;
pub const PAGE_ORDINAL: u32 = 0x0a;
pub const PAGE_PID: u32 = 0x0f;

/// Most usages or elements taken from one item.
const MAX_ELEMENTS: u32 = 4096;

/// A usage with its page in the high 16 bits.
pub const fn usage(page: u32, id: u32) -> u32 {
    page << 16 | id
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReportKind {
    Input,
    Output,
    Feature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub kind: ReportKind,
    pub report_id: u8,
    /// Offset of the first element in bits, not counting the report id byte.
    pub bit_offset: u32,
    pub size: u32,
    pub count: u32,
    /// Main item flags: bit 0 constant, bit 1 variable, bit 6 null state.
    pub flags: u32,
    /// Variable fields: one usage per element (the last repeats). Array fields: the
    /// selectable usages, selected by `logical_min + index`.
    pub usages: Vec<u32>,
    pub logical_min: i32,
    pub logical_max: i32,
    pub physical_min: i32,
    pub physical_max: i32,
    pub unit: u32,
    pub unit_exponent: i32,
    /// Usages of the enclosing collections, outermost first.
    pub collections: Vec<u32>,
}

impl Field {
    pub fn is_constant(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn is_array(&self) -> bool {
        self.flags & 2 == 0
    }

    pub fn has_null(&self) -> bool {
        self.flags & 0x40 != 0
    }

    /// Elements worth looking at (descriptors may declare absurd counts).
    pub fn elements(&self) -> usize {
        self.count.min(MAX_ELEMENTS) as usize
    }

    pub fn usage(&self, i: usize) -> Option<u32> {
        self.usages.get(i).or(self.usages.last()).copied()
    }

    /// Physical range, which equals the logical range when the descriptor gives none.
    pub fn physical(&self) -> (i32, i32) {
        if self.physical_min == 0 && self.physical_max == 0 {
            (self.logical_min, self.logical_max)
        } else {
            (self.physical_min, self.physical_max)
        }
    }

    pub fn has_physical(&self) -> bool {
        !(self.physical_min == 0 && self.physical_max == 0)
    }

    /// Logical value for a physical one, clamped to the logical range.
    pub fn to_logical(&self, phys: i64) -> i32 {
        let (pmin, pmax) = self.physical();
        let (lmin, lmax) = (self.logical_min as i64, self.logical_max as i64);
        if pmax == pmin {
            return lmin as i32;
        }
        let q = (phys - pmin as i64) as f64 * (lmax - lmin) as f64 / (pmax as i64 - pmin as i64) as f64;
        (lmin + q.round() as i64).clamp(lmin.min(lmax), lmin.max(lmax)) as i32
    }

    fn in_collection(&self, c: u32) -> bool {
        self.collections.contains(&c)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Descriptor {
    pub fields: Vec<Field>,
    /// Whether reports are prefixed with a report id byte.
    pub numbered: bool,
    /// Length in bits of each report (without the id byte).
    bits: BTreeMap<(ReportKind, u8), u32>,
    /// Usage of the first application collection.
    pub application: Option<u32>,
}

#[derive(Debug, Clone, Default)]
struct Globals {
    page: u32,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    unit: u32,
    unit_exponent: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

fn signed(data: u32, len: usize) -> i32 {
    match len {
        1 => data as u8 as i8 as i32,
        2 => data as u16 as i16 as i32,
        _ => data as i32,
    }
}

/// A maximum that only makes sense unsigned (e.g. `25 FF` after a minimum of 0).
fn max_value(min: i32, data: u32, len: usize) -> i32 {
    let v = signed(data, len);
    if v < min && min >= 0 { data as i32 } else { v }
}

impl Descriptor {
    pub fn parse(d: &[u8]) -> Result<Descriptor, String> {
        let mut out = Descriptor::default();
        let mut g = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        let mut usages: Vec<u32> = Vec::new();
        let mut usage_min: Option<u32> = None;
        let mut collections: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < d.len() {
            let prefix = d[i];
            if prefix == 0xfe {
                // Long item: data size, tag, data.
                let n = *d.get(i + 1).ok_or("truncated long item")? as usize;
                i += 3 + n;
                continue;
            }
            let len = [0, 1, 2, 4][(prefix & 3) as usize];
            if i + 1 + len > d.len() {
                return Err(format!("truncated item at {i}"));
            }
            let mut data = 0u32;
            for k in 0..len {
                data |= (d[i + 1 + k] as u32) << (8 * k);
            }
            let ty = (prefix >> 2) & 3;
            let tag = prefix >> 4;
            i += 1 + len;
            let full_usage = |data: u32| if len == 4 { data } else { usage(g.page, data) };
            match ty {
                0 => {
                    match tag {
                        0x8 | 0x9 | 0xb => {
                            let kind = match tag {
                                0x8 => ReportKind::Input,
                                0x9 => ReportKind::Output,
                                _ => ReportKind::Feature,
                            };
                            let key = (kind, g.report_id);
                            let off = out.bits.entry(key).or_insert(0);
                            let f = Field {
                                kind,
                                report_id: g.report_id,
                                bit_offset: *off,
                                size: g.size,
                                count: g.count,
                                flags: data,
                                usages: std::mem::take(&mut usages),
                                logical_min: g.logical_min,
                                logical_max: g.logical_max,
                                physical_min: g.physical_min,
                                physical_max: g.physical_max,
                                unit: g.unit,
                                unit_exponent: g.unit_exponent,
                                collections: collections.clone(),
                            };
                            *off = off.saturating_add(g.size.saturating_mul(g.count));
                            out.fields.push(f);
                        }
                        0xa => {
                            let u = usages.first().copied().unwrap_or(0);
                            if data == 1 && out.application.is_none() {
                                out.application = Some(u);
                            }
                            collections.push(u);
                            usages.clear();
                        }
                        0xc => {
                            collections.pop();
                            usages.clear();
                        }
                        _ => {}
                    }
                    usage_min = None;
                }
                1 => match tag {
                    0x0 => g.page = data,
                    0x1 => g.logical_min = signed(data, len),
                    0x2 => g.logical_max = max_value(g.logical_min, data, len),
                    0x3 => g.physical_min = signed(data, len),
                    0x4 => g.physical_max = max_value(g.physical_min, data, len),
                    0x5 => {
                        // Four-bit two's complement, though some descriptors use a byte.
                        let v = signed(data, len);
                        g.unit_exponent = if (8..16).contains(&v) { v - 16 } else { v };
                    }
                    0x6 => g.unit = data,
                    0x7 => g.size = data,
                    0x8 => {
                        g.report_id = data as u8;
                        out.numbered = true;
                    }
                    0x9 => g.count = data,
                    0xa => stack.push(g.clone()),
                    0xb => g = stack.pop().ok_or("pop without push")?,
                    _ => {}
                },
                2 => match tag {
                    0x0 => usages.push(full_usage(data)),
                    0x1 => usage_min = Some(full_usage(data)),
                    0x2 => {
                        let max = full_usage(data);
                        if let Some(min) = usage_min.take() {
                            // Bounded, so an odd descriptor cannot exhaust memory.
                            usages.extend(min..=max.max(min).min(min.saturating_add(MAX_ELEMENTS)));
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(out)
    }

    /// Report length in bytes, including the id byte of numbered reports.
    pub fn report_len(&self, kind: ReportKind, id: u8) -> usize {
        let bits = self.bits.get(&(kind, id)).copied().unwrap_or(0);
        bits.div_ceil(8) as usize + self.numbered as usize
    }

    /// Ids of the reports of a kind that exist.
    pub fn report_ids(&self, kind: ReportKind) -> Vec<u8> {
        self.bits
            .keys()
            .filter(|(k, _)| *k == kind)
            .map(|(_, id)| *id)
            .collect()
    }

    /// The variable field element carrying `usage` inside `collection` (0 for any).
    pub fn find(&self, kind: ReportKind, collection: u32, usage: u32) -> Option<(&Field, usize)> {
        self.fields
            .iter()
            .filter(|f| f.kind == kind && !f.is_array() && !f.is_constant())
            .filter(|f| collection == 0 || f.in_collection(collection))
            .find_map(|f| (0..f.elements()).find(|&i| f.usage(i) == Some(usage)).map(|i| (f, i)))
    }

    /// The array field that can select `usage` inside `collection` (0 for any).
    pub fn find_array(&self, kind: ReportKind, collection: u32, usage: u32) -> Option<&Field> {
        self.fields
            .iter()
            .filter(|f| f.kind == kind && f.is_array() && !f.is_constant())
            .filter(|f| collection == 0 || f.in_collection(collection))
            .find(|f| f.usages.contains(&usage))
    }

    pub fn new_report(&self, kind: ReportKind, id: u8) -> Report {
        let mut data = vec![0u8; self.report_len(kind, id).max(1)];
        if self.numbered {
            data[0] = id;
        }
        Report {
            numbered: self.numbered,
            data,
        }
    }
}

/// A report buffer, starting with the report id byte when reports are numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    numbered: bool,
    pub data: Vec<u8>,
}

impl Report {
    /// Wrap received bytes (which start with the id byte when reports are numbered).
    pub fn from_bytes(numbered: bool, data: Vec<u8>) -> Report {
        Report { numbered, data }
    }

    pub fn id(&self) -> u8 {
        if self.numbered { self.data[0] } else { 0 }
    }

    fn base(&self) -> usize {
        self.numbered as usize * 8
    }

    pub fn get_raw(&self, f: &Field, index: usize) -> u32 {
        let start = self.base() + f.bit_offset as usize + index * f.size as usize;
        let mut v = 0u32;
        for b in 0..(f.size as usize).min(32) {
            let bit = start + b;
            if let Some(byte) = self.data.get(bit / 8)
                && byte >> (bit % 8) & 1 != 0
            {
                v |= 1 << b;
            }
        }
        v
    }

    /// Element value, sign-extended when the logical range is signed.
    pub fn get(&self, f: &Field, index: usize) -> i32 {
        let v = self.get_raw(f, index);
        if f.logical_min < 0 && f.size < 32 && v & (1 << (f.size - 1)) != 0 {
            (v | !((1u32 << f.size) - 1)) as i32
        } else {
            v as i32
        }
    }

    pub fn set_raw(&mut self, f: &Field, index: usize, v: u32) {
        let start = self.base() + f.bit_offset as usize + index * f.size as usize;
        for b in 0..(f.size as usize).min(32) {
            let bit = start + b;
            if let Some(byte) = self.data.get_mut(bit / 8) {
                if v >> b & 1 != 0 {
                    *byte |= 1 << (bit % 8);
                } else {
                    *byte &= !(1 << (bit % 8));
                }
            }
        }
    }

    pub fn set(&mut self, f: &Field, index: usize, v: i32) {
        self.set_raw(f, index, v as u32);
    }
}

/// Parse "05 01 09 04 ..." into bytes.
pub fn parse_hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect()
}

/// The Microsoft SideWinder Force Feedback 2's report descriptor (1343 bytes), as
/// captured with a USB analyser and posted in the DescentBB forum thread "Reverse
/// Engineering the Force Feedback Pro".
#[cfg(test)]
pub(crate) fn sidewinder_ff2() -> Descriptor {
    Descriptor::parse(&parse_hex(include_str!("testdata/sidewinder_ff2.hex"))).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: u32 = usage(PAGE_GENERIC_DESKTOP, 0x30);

    #[test]
    fn sidewinder_input_report_layout() {
        let d = sidewinder_ff2();
        assert!(d.numbered);
        assert_eq!(d.application, Some(usage(PAGE_GENERIC_DESKTOP, 0x04)));
        assert_eq!(d.report_len(ReportKind::Input, 1), 13);
        assert_eq!(d.report_len(ReportKind::Input, 2), 3);
        let (x, _) = d.find(ReportKind::Input, 0, X).unwrap();
        assert_eq!((x.bit_offset, x.size, x.logical_min, x.logical_max), (0, 10, -512, 511));
        let (y, _) = d.find(ReportKind::Input, 0, usage(1, 0x31)).unwrap();
        assert_eq!(y.bit_offset, 16);
        let (rz, _) = d.find(ReportKind::Input, 0, usage(1, 0x35)).unwrap();
        assert_eq!((rz.bit_offset, rz.size, rz.logical_min), (32, 6, -32));
        let (sl, _) = d.find(ReportKind::Input, 0, usage(1, 0x36)).unwrap();
        assert_eq!((sl.bit_offset, sl.size, sl.logical_max), (40, 7, 127));
        let (hat, _) = d.find(ReportKind::Input, 0, usage(1, 0x39)).unwrap();
        assert_eq!(
            (hat.bit_offset, hat.size, hat.has_null(), hat.physical_max),
            (48, 4, true, 315)
        );
        let (b8, i) = d.find(ReportKind::Input, 0, usage(PAGE_BUTTON, 8)).unwrap();
        assert_eq!((b8.bit_offset, i), (56, 7));
    }

    #[test]
    fn sidewinder_pid_report_lengths() {
        let d = sidewinder_ff2();
        assert_eq!(
            d.report_ids(ReportKind::Output),
            (1..=14).filter(|&i| i != 9).collect::<Vec<u8>>()
        );
        assert_eq!(d.report_len(ReportKind::Output, 1), 16); // Set Effect
        assert_eq!(d.report_len(ReportKind::Output, 10), 4); // Effect Operation
        assert_eq!(d.report_len(ReportKind::Feature, 1), 4); // Create New Effect
        assert_eq!(d.report_len(ReportKind::Feature, 2), 5); // PID Block Load
        assert_eq!(d.report_len(ReportKind::Feature, 3), 5); // PID Pool
        // The direction ordinals of Set Effect sit in their own collection.
        let dir = usage(PAGE_PID, 0x57);
        let (f, i) = d.find(ReportKind::Output, dir, usage(PAGE_ORDINAL, 2)).unwrap();
        assert_eq!(
            (
                f.report_id,
                f.bit_offset + 8 * i as u32,
                f.physical_max,
                f.unit_exponent
            ),
            (1, 96, 36000, -2)
        );
        let ty = d
            .find_array(ReportKind::Feature, usage(PAGE_PID, 0xab), usage(PAGE_PID, 0x31))
            .unwrap();
        assert_eq!((ty.report_id, ty.logical_min, ty.usages.len()), (1, 1, 12));
    }

    #[test]
    fn signed_fields_round_trip() {
        let d = sidewinder_ff2();
        let (x, _) = d.find(ReportKind::Input, 0, X).unwrap();
        let mut r = d.new_report(ReportKind::Input, 1);
        r.set(x, 0, -512);
        assert_eq!(r.data[1..3], [0x00, 0x02]);
        assert_eq!(r.get(x, 0), -512);
        r.set(x, 0, 511);
        assert_eq!(r.get(x, 0), 511);
    }

    #[test]
    fn physical_to_logical_rounds_and_clamps() {
        let d = sidewinder_ff2();
        let set_effect = usage(PAGE_PID, 0x21);
        let (gain, _) = d.find(ReportKind::Output, set_effect, usage(PAGE_PID, 0x52)).unwrap();
        assert_eq!(gain.to_logical(10000), 255);
        assert_eq!(gain.to_logical(5000), 128);
        assert_eq!(gain.to_logical(20000), 255);
        let (mag, _) = d
            .find(ReportKind::Output, usage(PAGE_PID, 0x73), usage(PAGE_PID, 0x70))
            .unwrap();
        assert_eq!(
            (mag.to_logical(-10000), mag.to_logical(0), mag.to_logical(10000)),
            (-255, 0, 255)
        );
    }
}
