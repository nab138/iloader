//! DFU-mode firmware upload
//!
//! DFU uploads a firmware image via chunked `DNLOAD` control transfers
//! (`bmRequestType=0x21`, `bRequest=1`), with a trailing salted CRC-32 appended
//! to the final packet, followed by a zero-length `DNLOAD` and a device reset.

use super::{ControlSetup, RecoveryDevice, TRANSFER_SIZE_DFU, USB_TIMEOUT_MS};
use crate::{IdeviceError, services::restore::RestoreError};

/// The salted suffix folded into the CRC and appended after the image.
const DFU_XBUF: [u8; 12] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xAC, 0x05, 0x00, 0x01, 0x55, 0x46, 0x44, 0x10,
];

/// Uploads `buf` to a device in DFU/WTF mode.
pub(super) async fn send_buffer_dfu(
    dev: &mut RecoveryDevice,
    buf: &[u8],
) -> Result<(), IdeviceError> {
    let packet_size = TRANSFER_SIZE_DFU;

    // Confirm the device is in DFU IDLE (state 2); otherwise clear/abort.
    let state = dev
        .transport()
        .control_in(ControlSetup::new(0xA1, 5, 0, 0), 1, USB_TIMEOUT_MS)
        .await?;
    match state.first().copied() {
        Some(2) => {} // DFU IDLE
        Some(10) => {
            dev.transport()
                .control_out(ControlSetup::new(0x21, 4, 0, 0), &[], USB_TIMEOUT_MS)
                .await?;
            return Err(IdeviceError::Restore(RestoreError::Recovery(
                "DFU error state; issued CLRSTATUS".into(),
            )));
        }
        other => {
            dev.transport()
                .control_out(ControlSetup::new(0x21, 6, 0, 0), &[], USB_TIMEOUT_MS)
                .await?;
            return Err(IdeviceError::Restore(RestoreError::Recovery(format!(
                "unexpected DFU state {other:?}; issued ABORT"
            ))));
        }
    }

    let num_packets = buf.len().div_ceil(packet_size);
    let mut offset = 0usize;
    let mut packet_index: u16 = 0;

    while offset < buf.len() {
        let end = (offset + packet_size).min(buf.len());
        let chunk = &buf[offset..end];
        let is_last = end >= buf.len();

        if is_last {
            // CRC over the entire image, then over the salted suffix.
            let mut crc = crc32_zlib(0xFFFF_FFFF, buf);
            crc = crc32_zlib(crc, &DFU_XBUF);

            let mut crc_chunk = DFU_XBUF.to_vec();
            crc_chunk.extend_from_slice(&crc.to_le_bytes());

            if chunk.len() + crc_chunk.len() > packet_size {
                // The CRC would overflow the packet: send chunk then CRC separately.
                dev.transport()
                    .control_out(
                        ControlSetup::new(0x21, 1, packet_index, 0),
                        chunk,
                        USB_TIMEOUT_MS,
                    )
                    .await?;
                dev.transport()
                    .control_out(
                        ControlSetup::new(0x21, 1, packet_index, 0),
                        &crc_chunk,
                        USB_TIMEOUT_MS,
                    )
                    .await?;
            } else {
                let mut combined = chunk.to_vec();
                combined.extend_from_slice(&crc_chunk);
                dev.transport()
                    .control_out(
                        ControlSetup::new(0x21, 1, packet_index, 0),
                        &combined,
                        USB_TIMEOUT_MS,
                    )
                    .await?;
            }
        } else {
            dev.transport()
                .control_out(
                    ControlSetup::new(0x21, 1, packet_index, 0),
                    chunk,
                    USB_TIMEOUT_MS,
                )
                .await?;
        }

        offset = end;
        packet_index = packet_index.wrapping_add(1);
    }

    // Wait for the device to reach DFU MANIFEST-SYNC (state 5).
    while dev.dfu_status().await? != 5 {
        crate::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Zero-length DNLOAD to finish, drain a couple of status reads, then reset.
    dev.transport()
        .control_out(
            ControlSetup::new(0x21, 1, num_packets as u16, 0),
            &[],
            USB_TIMEOUT_MS,
        )
        .await?;
    let _ = dev.dfu_status().await?;
    let _ = dev.dfu_status().await?;
    dev.transport().reset().await?;

    Ok(())
}

fn crc32_zlib(mut crc: u32, data: &[u8]) -> u32 {
    crc ^= 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}
