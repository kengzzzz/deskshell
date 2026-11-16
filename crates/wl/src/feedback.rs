//! linux-dmabuf v4 feedback. Hyprland rejects `create` pairs outside it as a protocol error.

#[derive(Debug, Default, Clone)]
pub(crate) struct Feedback {
    table: Vec<(u32, u64)>,
    main_device: Option<u64>,
    tranches: Vec<Tranche>,
    pending: Vec<Tranche>,
    current: Tranche,
    complete: bool,
}

#[derive(Debug, Default, Clone)]
struct Tranche {
    device: Option<u64>,
    indices: Vec<u16>,
}

const ENTRY: usize = 16;

pub(crate) fn parse_table(bytes: &[u8]) -> Vec<(u32, u64)> {
    bytes
        .as_chunks::<ENTRY>()
        .0
        .iter()
        .map(|entry| {
            let [f0, f1, f2, f3, _, _, _, _, m0, m1, m2, m3, m4, m5, m6, m7] = *entry;
            (
                u32::from_ne_bytes([f0, f1, f2, f3]),
                u64::from_ne_bytes([m0, m1, m2, m3, m4, m5, m6, m7]),
            )
        })
        .collect()
}

pub(crate) fn parse_dev(bytes: &[u8]) -> Option<u64> {
    if let Some(raw) = bytes.as_array::<8>() {
        return Some(u64::from_ne_bytes(*raw));
    }
    bytes
        .as_array::<4>()
        .map(|raw| u64::from(u32::from_ne_bytes(*raw)))
}

pub(crate) fn parse_modifiers(bytes: &[u8], out: &mut Vec<u64>) {
    out.clear();
    out.extend(
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|raw| u64::from_ne_bytes(*raw)),
    );
}

impl Feedback {
    pub fn set_table(&mut self, table: Vec<(u32, u64)>) {
        self.table = table;
    }

    pub fn main_device(&mut self, bytes: &[u8]) {
        if self.complete {
            self.complete = false;
            self.pending.clear();
        }
        self.main_device = parse_dev(bytes);
    }

    pub fn tranche_device(&mut self, bytes: &[u8]) {
        self.current.device = parse_dev(bytes);
    }

    pub fn tranche_formats(&mut self, bytes: &[u8]) {
        let indices = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|raw| u16::from_ne_bytes(*raw));
        self.current.indices.extend(indices);
    }

    pub fn tranche_done(&mut self) {
        self.pending
            .push(std::mem::take(&mut self.current));
    }

    pub fn done(&mut self) {
        self.tranches = std::mem::take(&mut self.pending);
        self.complete = true;
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Indices outside the table are ignored.
    pub fn allows(&self, format: u32, modifier: u64) -> bool {
        self.main_tranches().any(|tranche| {
            tranche
                .indices
                .iter()
                .any(|&index| self.table.get(usize::from(index)) == Some(&(format, modifier)))
        })
    }

    pub fn modifiers(&self, format: u32, out: &mut Vec<u64>) {
        out.clear();
        for tranche in self.main_tranches() {
            for &index in &tranche.indices {
                if let Some(&(entry_format, modifier)) = self.table.get(usize::from(index))
                    && entry_format == format
                    && !out.contains(&modifier)
                {
                    out.push(modifier);
                }
            }
        }
    }

    fn main_tranches(&self) -> impl Iterator<Item = &Tranche> {
        let main = self.main_device;
        self.tranches
            .iter()
            .filter(move |tranche| main.is_some() && tranche.device == main)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ARGB8888, MOD_INVALID, MOD_LINEAR, XRGB8888};

    const NV_BLOCK: u64 = 0x0300_0000_0060_6014;

    fn table_bytes(entries: &[(u32, u64)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for &(format, modifier) in entries {
            bytes.extend_from_slice(&format.to_ne_bytes());
            bytes.extend_from_slice(&[0xaa; 4]);
            bytes.extend_from_slice(&modifier.to_ne_bytes());
        }
        bytes
    }

    fn indices(list: &[u16]) -> Vec<u8> {
        list.iter()
            .flat_map(|index| index.to_ne_bytes())
            .collect()
    }

    #[test]
    fn parses_sixteen_byte_entries() {
        let bytes = table_bytes(&[(ARGB8888, NV_BLOCK), (XRGB8888, MOD_LINEAR)]);
        assert_eq!(
            parse_table(&bytes),
            vec![(ARGB8888, NV_BLOCK), (XRGB8888, MOD_LINEAR)]
        );
        assert_eq!(
            parse_table(&bytes[..20]),
            vec![(ARGB8888, NV_BLOCK)]
        );
    }

    #[test]
    fn selects_the_main_device_tranche() {
        let main = 0xe280_u64;
        let other = 0xe281_u64;
        let mut feedback = Feedback::default();
        feedback.set_table(parse_table(&table_bytes(&[
            (ARGB8888, NV_BLOCK),
            (ARGB8888, MOD_INVALID),
            (XRGB8888, MOD_LINEAR),
            (ARGB8888, MOD_LINEAR),
        ])));
        feedback.main_device(&main.to_ne_bytes());
        feedback.tranche_device(&other.to_ne_bytes());
        feedback.tranche_formats(&indices(&[3]));
        feedback.tranche_done();
        feedback.tranche_device(&main.to_ne_bytes());
        feedback.tranche_formats(&indices(&[0, 1, 2, 99]));
        feedback.tranche_done();
        assert!(!feedback.is_complete());
        feedback.done();
        assert!(feedback.is_complete());

        let mut mods = Vec::new();
        feedback.modifiers(ARGB8888, &mut mods);
        assert_eq!(mods, vec![NV_BLOCK, MOD_INVALID]);
        assert!(feedback.allows(XRGB8888, MOD_LINEAR));
        assert!(
            !feedback.allows(ARGB8888, MOD_LINEAR),
            "only listed on the other device"
        );
    }

    #[test]
    fn a_new_batch_replaces_the_tranches() {
        let main = 7_u64;
        let mut feedback = Feedback::default();
        feedback.set_table(vec![(ARGB8888, NV_BLOCK), (ARGB8888, MOD_LINEAR)]);
        feedback.main_device(&main.to_ne_bytes());
        feedback.tranche_device(&main.to_ne_bytes());
        feedback.tranche_formats(&indices(&[0]));
        feedback.tranche_done();
        feedback.done();
        assert!(feedback.allows(ARGB8888, NV_BLOCK));

        feedback.set_table(vec![(ARGB8888, MOD_LINEAR)]);
        feedback.main_device(&main.to_ne_bytes());
        feedback.tranche_device(&main.to_ne_bytes());
        feedback.tranche_formats(&indices(&[0]));
        feedback.tranche_done();
        feedback.done();
        assert!(!feedback.allows(ARGB8888, NV_BLOCK));
        assert!(feedback.allows(ARGB8888, MOD_LINEAR));
    }

    #[test]
    fn parses_devices_and_modifier_arrays() {
        assert_eq!(parse_dev(&0x1234_u64.to_ne_bytes()), Some(0x1234));
        assert_eq!(parse_dev(&[1, 2, 3]), None);
        let mut mods = Vec::new();
        let bytes: Vec<u8> = [NV_BLOCK, MOD_LINEAR]
            .iter()
            .flat_map(|m| m.to_ne_bytes())
            .collect();
        parse_modifiers(&bytes, &mut mods);
        assert_eq!(mods, vec![NV_BLOCK, MOD_LINEAR]);
    }
}
