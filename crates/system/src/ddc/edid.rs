//! The parts of an EDID base block that detection needs (VESA E-EDID 1.4).

pub const BLOCK: usize = 128;
const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
const DESCRIPTORS: [usize; 4] = [54, 72, 90, 108];
const PRODUCT_NAME: u8 = 0xFC;

pub fn is_base_block(block: &[u8]) -> bool {
    block.len() >= BLOCK
        && block[..8] == HEADER
        && block[..BLOCK]
            .iter()
            .fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
            == 0
}

/// Parsed as ddcutil's `get_edid_descriptor_strings` does, so the last 0xFC descriptor wins.
pub fn model_name(block: &[u8]) -> Option<String> {
    if !is_base_block(block) {
        return None;
    }
    DESCRIPTORS
        .iter()
        .map(|&offset| &block[offset..offset + 18])
        .rfind(|descriptor| {
            descriptor[..3] == [0, 0, 0] && descriptor[4] == 0 && descriptor[3] == PRODUCT_NAME
        })
        .map(|descriptor| {
            let text = &descriptor[5..];
            let end = text
                .iter()
                .position(|&byte| byte == b'\n')
                .unwrap_or(text.len());
            String::from_utf8_lossy(&text[..end])
                .trim_end()
                .to_owned()
        })
}

#[cfg(test)]
pub(crate) fn fixture(name: &str, serial: u8) -> [u8; BLOCK] {
    let mut block = [0_u8; BLOCK];
    block[..8].copy_from_slice(&HEADER);
    block[8..12].copy_from_slice(&[0x52, 0x94, 0x34, 0x12]);
    block[12] = serial;
    block[18] = 1;
    block[19] = 4;
    // A detailed timing descriptor first, so the name is not in the first slot.
    block[54] = 0x01;
    block[55] = 0x1D;
    let descriptor = &mut block[72..90];
    descriptor[3] = PRODUCT_NAME;
    let text = &mut descriptor[5..];
    text.fill(b' ');
    let bytes = name.as_bytes();
    let len = bytes.len().min(13);
    text[..len].copy_from_slice(&bytes[..len]);
    if len < 13 {
        text[len] = b'\n';
    }
    let sum = block[..127]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    block[127] = 0_u8.wrapping_sub(sum);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_name() {
        let block = fixture("TEST 27Q", 1);
        assert!(is_base_block(&block));
        assert_eq!(model_name(&block).as_deref(), Some("TEST 27Q"));
        assert_eq!(
            model_name(&fixture("ABCDEFGHIJKLM", 2)).as_deref(),
            Some("ABCDEFGHIJKLM")
        );
    }

    #[test]
    fn rejects_bad_blocks() {
        let mut block = fixture("TEST 27Q", 1);
        block[127] ^= 1;
        assert!(!is_base_block(&block));
        assert_eq!(model_name(&block), None);
        assert!(!is_base_block(&[0; BLOCK]));
        assert!(!is_base_block(&block[..64]));
    }

    #[test]
    fn no_name_descriptor() {
        let mut block = fixture("X", 1);
        block[75] = 0xFF;
        block[127] = block[127].wrapping_sub(0xFF - PRODUCT_NAME);
        assert!(is_base_block(&block));
        assert_eq!(model_name(&block), None);
    }
}
