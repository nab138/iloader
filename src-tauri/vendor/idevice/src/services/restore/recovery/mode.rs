use std::collections::HashMap;

/// `IBFL` bit indicating the iBoot understands IMG4.
const IBOOT_FLAG_IMAGE4_AWARE: u64 = 1 << 2;

/// A recovery-family USB mode, keyed by `idProduct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Recovery mode, `idProduct` 0x1280.
    Recovery1,
    /// Recovery mode, `idProduct` 0x1281.
    Recovery2,
    /// Recovery mode, `idProduct` 0x1282.
    Recovery3,
    /// Recovery mode, `idProduct` 0x1283.
    Recovery4,
    /// WTF mode, `idProduct` 0x1222.
    Wtf,
    /// DFU mode, `idProduct` 0x1227.
    Dfu,
}

impl Mode {
    /// Maps a USB `idProduct` to a mode, if it is one of Apple's recovery modes.
    pub fn from_product_id(product_id: u16) -> Option<Self> {
        Some(match product_id {
            0x1280 => Mode::Recovery1,
            0x1281 => Mode::Recovery2,
            0x1282 => Mode::Recovery3,
            0x1283 => Mode::Recovery4,
            0x1222 => Mode::Wtf,
            0x1227 => Mode::Dfu,
            _ => return None,
        })
    }

    /// The USB `idProduct` for this mode.
    pub fn product_id(self) -> u16 {
        match self {
            Mode::Recovery1 => 0x1280,
            Mode::Recovery2 => 0x1281,
            Mode::Recovery3 => 0x1282,
            Mode::Recovery4 => 0x1283,
            Mode::Wtf => 0x1222,
            Mode::Dfu => 0x1227,
        }
    }

    /// Whether this is a recovery (iBoot) mode, as opposed to DFU/WTF.
    pub fn is_recovery(self) -> bool {
        !matches!(self, Mode::Wtf | Mode::Dfu)
    }
}

/// Identifiers parsed from a recovery/DFU device's USB serial-number string.
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    /// Chip ID (`CPID`).
    pub cpid: Option<u64>,
    /// Board ID (`BDID`).
    pub bdid: Option<u64>,
    /// Exclusive chip ID (`ECID`).
    pub ecid: Option<u64>,
    /// iBoot flags (`IBFL`).
    pub ibfl: Option<u64>,
    /// Serial number (`SRNM`, brackets stripped).
    pub srnm: Option<String>,
    /// iBoot version tag (`SRTG`, brackets stripped).
    pub srtg: Option<String>,
    /// AP nonce (`NONC`).
    pub ap_nonce: Option<Vec<u8>>,
    /// SEP nonce (`SNON`).
    pub sep_nonce: Option<Vec<u8>>,
    /// All raw key/value pairs.
    pub raw: HashMap<String, String>,
}

impl DeviceInfo {
    /// Parses a serial-number string like
    /// `"CPID:8010 BDID:08 ECID:00.. IBFL:3C SRNM:[..] SRTG:[iBoot-..] NONC:.."`.
    pub fn parse(serial: &str) -> Self {
        let mut info = DeviceInfo::default();
        for component in serial.split(' ') {
            let Some((key, value)) = component.split_once(':') else {
                continue;
            };
            let mut value = value.to_string();
            if (key == "SRNM" || key == "SRTG") && value.starts_with('[') && value.ends_with(']') {
                value = value[1..value.len() - 1].to_string();
            }
            info.raw.insert(key.to_string(), value.clone());
            match key {
                "CPID" => info.cpid = u64::from_str_radix(&value, 16).ok(),
                "BDID" => info.bdid = u64::from_str_radix(&value, 16).ok(),
                "ECID" => info.ecid = u64::from_str_radix(&value, 16).ok(),
                "IBFL" => info.ibfl = u64::from_str_radix(&value, 16).ok(),
                "SRNM" => info.srnm = Some(value),
                "SRTG" => info.srtg = Some(value),
                "NONC" => info.ap_nonce = hex_decode(&value),
                "SNON" => info.sep_nonce = hex_decode(&value),
                _ => {}
            }
        }
        info
    }

    /// Whether the iBoot is IMG4-aware (from `IBFL`).
    pub fn is_image4_supported(&self) -> bool {
        self.ibfl
            .map(|f| f & IBOOT_FLAG_IMAGE4_AWARE != 0)
            .unwrap_or(false)
    }
}

/// Decodes a hex string into bytes, or `None` if malformed.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
