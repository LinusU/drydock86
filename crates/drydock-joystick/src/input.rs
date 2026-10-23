//! Reading a host game controller's input reports into joystick state.

use crate::hid::{Descriptor, Field, PAGE_BUTTON, PAGE_GENERIC_DESKTOP, Report, ReportKind, usage};

/// Centred point-of-view hat, as DirectInput reports it.
pub const POV_CENTERED: u32 = u32::MAX;

/// Controller state in the emulated joystick's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoyState {
    /// X, Y, throttle, twist (rudder), each 0..=65535.
    pub axes: [u16; 4],
    /// Bit n is button n+1.
    pub buttons: u32,
    /// Hundredths of a degree clockwise from north, or [`POV_CENTERED`].
    pub pov: u32,
}

impl Default for JoyState {
    fn default() -> Self {
        JoyState {
            axes: [32768; 4],
            buttons: 0,
            pov: POV_CENTERED,
        }
    }
}

/// Where the controls are in the controller's input reports.
#[derive(Debug, Clone)]
pub struct Layout {
    axes: [Option<(Field, usize)>; 4],
    hat: Option<(Field, usize)>,
    buttons: Vec<(Field, usize, u32)>,
}

fn gd(id: u32) -> u32 {
    usage(PAGE_GENERIC_DESKTOP, id)
}

impl Layout {
    pub fn new(d: &Descriptor) -> Layout {
        let find = |ids: &[u32]| {
            ids.iter()
                .find_map(|&id| d.find(ReportKind::Input, 0, gd(id)))
                .map(|(f, i)| (f.clone(), i))
        };
        // Throttle: a slider, else Z, else a dial. Twist: Rz, else Rx.
        let axes = [
            find(&[0x30]),
            find(&[0x31]),
            find(&[0x36, 0x32, 0x37]),
            find(&[0x35, 0x33]),
        ];
        let hat = find(&[0x39]);
        let mut buttons = Vec::new();
        for f in d
            .fields
            .iter()
            .filter(|f| f.kind == ReportKind::Input && !f.is_constant())
        {
            if f.is_array() {
                continue;
            }
            for i in 0..f.elements() {
                if let Some(u) = f.usage(i)
                    && u >> 16 == PAGE_BUTTON
                    && (1..=32).contains(&(u & 0xffff))
                {
                    buttons.push((f.clone(), i, (u & 0xffff) - 1));
                }
            }
        }
        Layout { axes, hat, buttons }
    }

    pub fn axis_count(&self) -> usize {
        self.axes.iter().flatten().count()
    }

    pub fn button_count(&self) -> usize {
        self.buttons.len()
    }

    pub fn has_hat(&self) -> bool {
        self.hat.is_some()
    }

    /// Update `st` from one input report; fields of other reports keep their values.
    pub fn decode(&self, r: &Report, st: &mut JoyState) {
        let id = r.id();
        for (k, a) in self.axes.iter().enumerate() {
            if let Some((f, i)) = a
                && f.report_id == id
            {
                let span = (f.logical_max as i64 - f.logical_min as i64).max(1);
                let v = (r.get(f, *i) as i64 - f.logical_min as i64).clamp(0, span);
                st.axes[k] = (v * 65535 / span) as u16;
            }
        }
        if let Some((f, i)) = &self.hat
            && f.report_id == id
        {
            let v = r.get(f, *i);
            st.pov = if v < f.logical_min || v > f.logical_max {
                POV_CENTERED
            } else {
                let steps = (f.logical_max - f.logical_min + 1) as u32;
                (v - f.logical_min) as u32 * (36000 / steps)
            };
        }
        for (f, i, bit) in &self.buttons {
            if f.report_id == id {
                if r.get_raw(f, *i) != 0 {
                    st.buttons |= 1 << bit;
                } else {
                    st.buttons &= !(1 << bit);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::sidewinder_ff2;

    #[test]
    fn sidewinder_report() {
        let d = sidewinder_ff2();
        let l = Layout::new(&d);
        assert_eq!((l.axis_count(), l.button_count(), l.has_hat()), (4, 8, true));
        // X full left (-512), Y centre (0), twist full right (31), throttle 127,
        // hat 2 (east), buttons 1 and 8.
        let mut bytes = vec![1u8, 0x00, 0x02, 0x00, 0x00, 31, 127, 0x02, 0b1000_0001, 0, 0, 0, 0];
        let mut st = JoyState::default();
        l.decode(&Report::from_bytes(true, bytes.clone()), &mut st);
        assert_eq!(st.axes, [0, 32799, 65535, 65535]);
        assert_eq!(st.pov, 9000);
        assert_eq!(st.buttons, 0b1000_0001);
        // Hat out of range is centred; the PID state report changes nothing.
        bytes[7] = 0x08;
        l.decode(&Report::from_bytes(true, bytes), &mut st);
        assert_eq!(st.pov, POV_CENTERED);
        let before = st;
        l.decode(&Report::from_bytes(true, vec![2, 0x03, 0x85]), &mut st);
        assert_eq!(st, before);
    }
}
