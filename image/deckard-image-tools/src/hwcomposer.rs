use sha2::{Digest, Sha256};

/// (file offset, stock bytes, patched bytes).
const PATCHES: [(usize, [u8; 4], [u8; 4]); 5] = [
    // hwc_binder_thread: configureRpcThreadpool(1) → (4), so it can't shrink
    // the pool the service already started.
    (0x3eee0, [0x20, 0x00, 0x80, 0x52], [0x80, 0x00, 0x80, 0x52]),
    // hwc_wayland_thread: on a lost connection, `bl abort` → `b 0x52db4`.
    (0x429b4, [0xe7, 0xa3, 0x00, 0x94], [0x00, 0x41, 0x00, 0x14]),
    // libffi's alignment nops before a jump table, unreachable:
    // exit_group(0).
    (0x52db4, [0x1f, 0x20, 0x03, 0xd5], [0xc8, 0x0b, 0x80, 0xd2]),
    (0x52db8, [0x1f, 0x20, 0x03, 0xd5], [0xe0, 0x03, 0x1f, 0xaa]),
    (0x52dbc, [0x1f, 0x20, 0x03, 0xd5], [0x01, 0x00, 0x00, 0xd4]),
];
const STOCK_SHA256: &str = "017cefa35bccc2d8ac7bec92dabeb65422c777229a787ed395bdb9c1b9015247";
const PATCHED_SHA256: &str = "4d58c41c721728449756481973a2ae7ba714321e6139702f50ddbca90fe447f2";

/// The patched composer HAL from the stock one.
///
/// # Errors
/// `stock` is not the expected build, or the result's digest is wrong.
pub fn patch(stock: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if format!("{:x}", Sha256::digest(stock)) != STOCK_SHA256 {
        return Err("hwcomposer: not the expected vendor build".into());
    }
    let mut patched = stock.to_vec();
    for (offset, before, after) in PATCHES {
        let instruction = patched
            .get_mut(offset..offset + before.len())
            .ok_or("hwcomposer: vendor image is too short")?;
        if instruction != before {
            return Err("hwcomposer: unexpected instruction at patch offset".into());
        }
        instruction.copy_from_slice(&after);
    }
    if format!("{:x}", Sha256::digest(&patched)) != PATCHED_SHA256 {
        return Err("hwcomposer: patched digest mismatch".into());
    }
    Ok(patched)
}

/// `patched` with the stock instructions put back.
#[cfg(test)]
fn unpatch(patched: &[u8]) -> Vec<u8> {
    let mut stock = patched.to_vec();
    for (offset, before, _) in PATCHES {
        stock[offset..offset + before.len()].copy_from_slice(&before);
    }
    stock
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
        let stock = unpatch(golden);
        assert_eq!(format!("{:x}", Sha256::digest(&stock)), STOCK_SHA256);
        assert_eq!(patch(&stock).unwrap(), golden);
    }

    #[test]
    fn rejects_unknown_or_already_patched_images() {
        let golden = include_bytes!("../../overlay/vendor/lib64/hw/hwcomposer.waydroid.so");
        assert!(patch(golden).is_err());
        let mut stock = unpatch(golden);
        stock[0] ^= 1;
        assert!(patch(&stock).is_err());
        assert!(patch(&[]).is_err());
    }
}
