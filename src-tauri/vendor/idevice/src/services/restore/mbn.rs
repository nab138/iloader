//! Baseband firmware (MBN) stitching

use tracing::{debug, warn};

use crate::{IdeviceError, services::restore::RestoreError};

const MBN_V1_MAGIC: &[u8] = &[0x0a, 0x00, 0x00, 0x00];
const MBN_V2_MAGIC: &[u8] = &[0xd1, 0xdc, 0x4b, 0x84, 0x34, 0x10, 0xd7, 0x73];
const MBN_BIN_MAGIC: &[u8] = &[0x04, 0x00, 0xea, 0x6c, 0x69, 0x48, 0x55];
const MBN_BIN_MAGIC_OFFSET: usize = 1;

/// Header sizes (bytes) of the recognized container formats.
const MBN_V1_HEADER: usize = 40; // 10 * u32
const MBN_V2_HEADER: usize = 56; // 14 * u32

/// Reads a little-endian `u32` at `offset`, if in range.
fn read_u32_le(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Best-effort detection of the "real" image size (excluding trailing signature),
/// used only to warn on a size mismatch.
fn detect_image_size(data: &[u8]) -> Option<usize> {
    if data.len() > MBN_V2_MAGIC.len() && data.starts_with(MBN_V2_MAGIC) {
        // MBN v2: data_size field at u32 index 10.
        let data_size = read_u32_le(data, 10 * 4)? as usize;
        Some(data_size + MBN_V2_HEADER)
    } else if data.len() > MBN_V1_MAGIC.len() && data.starts_with(MBN_V1_MAGIC) {
        // MBN v1: data_size field at u32 index 8.
        let data_size = read_u32_le(data, 8 * 4)? as usize;
        Some(data_size + MBN_V1_HEADER)
    } else if data.len() > MBN_BIN_MAGIC.len() + MBN_BIN_MAGIC_OFFSET
        && data[MBN_BIN_MAGIC_OFFSET..MBN_BIN_MAGIC_OFFSET + MBN_BIN_MAGIC.len()] == *MBN_BIN_MAGIC
    {
        // MBN BIN: total_size at u32 index 1.
        read_u32_le(data, 4).map(|s| s as usize)
    } else {
        None
    }
}

/// Overwrites the trailing signature region of `data` with `blob`.
///
/// Returns `data` with its final `blob.len()` bytes replaced by `blob`.
///
/// # Errors
/// Returns [`IdeviceError::Restore`] if either input is empty or `blob` is larger
/// than `data`.
pub fn mbn_stitch(data: &[u8], blob: &[u8]) -> Result<Vec<u8>, IdeviceError> {
    if data.is_empty() {
        return Err(IdeviceError::Restore(RestoreError::Baseband(
            "mbn_stitch: data is empty".into(),
        )));
    }
    if blob.is_empty() {
        return Err(IdeviceError::Restore(RestoreError::Baseband(
            "mbn_stitch: blob is empty".into(),
        )));
    }
    if blob.len() > data.len() {
        return Err(IdeviceError::Restore(RestoreError::Baseband(format!(
            "mbn_stitch: blob ({}) larger than data ({})",
            blob.len(),
            data.len()
        ))));
    }

    if let Some(parsed) = detect_image_size(data)
        && parsed != data.len()
    {
        warn!(
            "mbn_stitch: size mismatch, header says {parsed:#x}, input is {:#x}",
            data.len()
        );
    }

    let stitch_offset = data.len() - blob.len();
    debug!(
        "mbn_stitch: stitching at {stitch_offset:#x}, size {:#x}",
        blob.len()
    );
    let mut out = data.to_vec();
    out[stitch_offset..].copy_from_slice(blob);
    Ok(out)
}
