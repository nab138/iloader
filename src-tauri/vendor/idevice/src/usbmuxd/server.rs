//! Server-side usbmuxd protocol messages.
//!
//! Only the standard usbmuxd message types are modeled here. Unknown
//! `MessageType`s decode to [`UsbmuxdError::UnknownMessageType`] so a
//! consumer can layer its own extensions on top.

use super::{RawPacket, UsbmuxdConnection, errors::UsbmuxdError};
use crate::IdeviceError;

/// A request sent by a usbmuxd client to the muxer.
#[derive(Debug, Clone)]
pub enum UsbmuxdServerRequest {
    /// List every attached device.
    ListDevices,
    /// Subscribe to attach/detach events for the life of the connection.
    Listen,
    /// Read this host's BUID.
    ReadBuid,
    /// Read a stored pairing record by UDID.
    ReadPairRecord {
        /// `PairRecordID` — the device UDID.
        pair_record_id: String,
    },
    /// Persist a pairing record.
    SavePairRecord {
        /// `PairRecordID` if the client supplied one.
        pair_record_id: Option<String>,
        /// `DeviceID` fallback used when `PairRecordID` is absent.
        device_id: Option<u64>,
        /// Raw serialized pairing record (`PairRecordData`).
        pair_record_data: Vec<u8>,
    },
    /// Open a connection to a port on a device.
    Connect {
        /// usbmuxd-assigned `DeviceID`.
        device_id: u64,
        /// Target port in host byte order. The wire carries `PortNumber` in
        /// network byte order; this value is already converted back.
        port: u16,
    },
}

impl UsbmuxdServerRequest {
    /// Decodes a client request from a received packet.
    ///
    /// A non-standard `MessageType` surfaces as
    /// [`UsbmuxdError::UnknownMessageType`] (wrapped in [`IdeviceError`]).
    pub fn decode(packet: &RawPacket) -> Result<Self, IdeviceError> {
        let plist = &packet.plist;
        let message_type = match plist.get("MessageType") {
            Some(plist::Value::String(s)) => s.as_str(),
            Some(_) => return Err(UsbmuxdError::UnexpectedFieldType("MessageType").into()),
            None => return Err(UsbmuxdError::MissingField("MessageType").into()),
        };

        Ok(match message_type {
            "ListDevices" => Self::ListDevices,
            "Listen" => Self::Listen,
            "ReadBUID" => Self::ReadBuid,
            "ReadPairRecord" => Self::ReadPairRecord {
                pair_record_id: get_string(plist, "PairRecordID")?,
            },
            "SavePairRecord" => {
                let pair_record_data = match plist.get("PairRecordData") {
                    Some(plist::Value::Data(d)) => d.clone(),
                    Some(_) => {
                        return Err(UsbmuxdError::UnexpectedFieldType("PairRecordData").into());
                    }
                    None => return Err(UsbmuxdError::MissingField("PairRecordData").into()),
                };
                let pair_record_id = match plist.get("PairRecordID") {
                    Some(plist::Value::String(s)) => Some(s.clone()),
                    _ => None,
                };
                let device_id = match plist.get("DeviceID") {
                    Some(plist::Value::Integer(i)) => i.as_unsigned(),
                    _ => None,
                };
                Self::SavePairRecord {
                    pair_record_id,
                    device_id,
                    pair_record_data,
                }
            }
            "Connect" => Self::Connect {
                device_id: get_unsigned(plist, "DeviceID")?,
                port: get_port(plist)?,
            },
            other => return Err(UsbmuxdError::UnknownMessageType(other.to_string()).into()),
        })
    }
}

/// A response (or broadcast event) the muxer sends back to a client.
#[derive(Debug, Clone)]
pub enum UsbmuxdServerResponse {
    /// `{ MessageType: Result, Number: n }`. `0` is success.
    Result(u32),
    /// `{ DeviceList: [...] }` - reply to [`UsbmuxdServerRequest::ListDevices`].
    DeviceList(Vec<plist::Value>),
    /// `{ PairRecordData: <data> }` - reply to a `ReadPairRecord`.
    PairRecord(Vec<u8>),
    /// `{ BUID: <string> }` - reply to a `ReadBUID`.
    Buid(String),
    /// An `Attached` broadcast. The dictionary is the full Attached payload
    /// (`DeviceID`, `MessageType`, `Properties`).
    Attached(plist::Dictionary),
    /// A `Detached` broadcast for the given `DeviceID`.
    Detached(u64),
}

impl UsbmuxdServerResponse {
    /// Builds the plist body for this response.
    pub fn into_dictionary(self) -> plist::Dictionary {
        match self {
            Self::Result(number) => crate::plist!(dict {
                "MessageType": "Result",
                "Number": number,
            }),
            Self::DeviceList(devices) => crate::plist!(dict {
                "DeviceList": devices,
            }),
            Self::PairRecord(data) => crate::plist!(dict {
                "PairRecordData": plist::Value::Data(data),
            }),
            Self::Buid(buid) => crate::plist!(dict {
                "BUID": buid,
            }),
            Self::Attached(payload) => payload,
            Self::Detached(device_id) => crate::plist!(dict {
                "MessageType": "Detached",
                "DeviceID": device_id,
            }),
        }
    }

    /// Wraps this response in a [`RawPacket`] ready to write to the client.
    pub fn into_packet(self, tag: u32) -> RawPacket {
        RawPacket::new(
            self.into_dictionary(),
            UsbmuxdConnection::XML_PLIST_VERSION,
            UsbmuxdConnection::PLIST_MESSAGE_TYPE,
            tag,
        )
    }
}

fn get_string(plist: &plist::Dictionary, key: &'static str) -> Result<String, UsbmuxdError> {
    match plist.get(key) {
        Some(plist::Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(UsbmuxdError::UnexpectedFieldType(key)),
        None => Err(UsbmuxdError::MissingField(key)),
    }
}

fn get_unsigned(plist: &plist::Dictionary, key: &'static str) -> Result<u64, UsbmuxdError> {
    match plist.get(key) {
        Some(plist::Value::Integer(i)) => i
            .as_unsigned()
            .ok_or(UsbmuxdError::UnexpectedFieldType(key)),
        Some(_) => Err(UsbmuxdError::UnexpectedFieldType(key)),
        None => Err(UsbmuxdError::MissingField(key)),
    }
}

/// Reads `PortNumber` and converts it from the wire's network byte order to
/// host order. The client encodes the port with `to_be()`, so we undo that
/// here. `PortNumber` may arrive as either a signed or unsigned plist integer.
fn get_port(plist: &plist::Dictionary) -> Result<u16, UsbmuxdError> {
    let raw = match plist.get("PortNumber") {
        Some(plist::Value::Integer(i)) => {
            if let Some(u) = i.as_unsigned() {
                u as u16
            } else if let Some(s) = i.as_signed() {
                s as u16
            } else {
                return Err(UsbmuxdError::UnexpectedFieldType("PortNumber"));
            }
        }
        Some(_) => return Err(UsbmuxdError::UnexpectedFieldType("PortNumber")),
        None => return Err(UsbmuxdError::MissingField("PortNumber")),
    };
    Ok(raw.to_be())
}
