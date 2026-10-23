use sha2::{Digest, Sha256};

const OFFSET: usize = 0x3eee0;
const STOCK_INSTRUCTION: [u8; 4] = [0x20, 0x00, 0x80, 0x52];
const PATCHED_INSTRUCTION: [u8; 4] = [0x80, 0x00, 0x80, 0x52];
const STOCK_SHA256: &str = "017cefa35bccc2d8ac7bec92dabeb65422c777229a787ed395bdb9c1b9015247";
const PATCHED_SHA256: &str = "42bef881c0fd2024d5438942eb3a070d8b486eebce28c5f4c9835b95eabf7ce0";

pub fn patch(stock: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if format!("{:x}", Sha256::digest(stock)) != STOCK_SHA256 {
        return Err("hwcomposer: not the expected vendor build".into());
    }
    let mut patched = stock.to_vec();
    let instruction = patched
        .get_mut(OFFSET..OFFSET + STOCK_INSTRUCTION.len())
        .ok_or("hwcomposer: vendor image is too short")?;
    if instruction != STOCK_INSTRUCTION {
        return Err("hwcomposer: unexpected instruction at patch offset".into());
    }
    instruction.copy_from_slice(&PATCHED_INSTRUCTION);
    if format!("{:x}", Sha256::digest(&patched)) != PATCHED_SHA256 {
        return Err("hwcomposer: patched digest mismatch".into());
    }
    Ok(patched)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests compare checked-in image fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn reconstructing_stock_produces_the_golden_patched_image() {
        let golden = include_bytes!("../../overlay/vendor/lib64/hw/hwcomposer.waydroid.so");
        assert_eq!(format!("{:x}", Sha256::digest(golden)), PATCHED_SHA256);
        let mut stock = golden.to_vec();
        stock[OFFSET..OFFSET + 4].copy_from_slice(&STOCK_INSTRUCTION);
        assert_eq!(format!("{:x}", Sha256::digest(&stock)), STOCK_SHA256);
        assert_eq!(patch(&stock).unwrap(), golden);
    }

    #[test]
    fn rejects_unknown_or_already_patched_images() {
        let golden = include_bytes!("../../overlay/vendor/lib64/hw/hwcomposer.waydroid.so");
        assert!(patch(golden).is_err());
        let mut stock = golden.to_vec();
        stock[OFFSET..OFFSET + 4].copy_from_slice(&STOCK_INSTRUCTION);
        stock[0] ^= 1;
        assert!(patch(&stock).is_err());
        assert!(patch(&[]).is_err());
    }
}
