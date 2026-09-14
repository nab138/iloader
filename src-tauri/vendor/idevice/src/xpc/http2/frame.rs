// Jackson Coxson

use crate::IdeviceError;
use crate::xpc::errors::XpcError;

/// Fixed HTTP/2 frame header size: 3-byte length, 1-byte type, 1-byte flags,
/// 4-byte stream id.
const FRAME_HEADER_LEN: usize = 9;

pub trait HttpFrame {
    fn serialize(&self) -> Vec<u8>;
}

#[derive(Debug)]
#[allow(dead_code)] // we don't care about frames from the device
pub enum Frame {
    Settings(SettingsFrame),
    WindowUpdate(WindowUpdateFrame),
    Headers(HeadersFrame),
    Data(DataFrame),
}

impl Frame {
    /// Parse a single frame from the front of `buf`.
    ///
    /// Returns `Ok(None)` when `buf` does not yet hold a complete frame (the
    /// caller should read more bytes and retry). On success returns the frame
    /// and how many bytes it consumed, so the caller can drain exactly that much
    /// — this keeps frame reassembly cancellation-safe: a partially-received
    /// frame stays buffered until it is whole. RST_STREAM and GOAWAY surface as
    /// errors (they consume no bytes; the connection is finished either way).
    pub fn parse(buf: &[u8]) -> Result<Option<(Self, usize)>, IdeviceError> {
        if buf.len() < FRAME_HEADER_LEN {
            return Ok(None);
        }
        let frame_len = u32::from_be_bytes([0x00, buf[0], buf[1], buf[2]]) as usize;
        let frame_type = buf[3];
        let flags = buf[4];
        let stream_id = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);

        let total = FRAME_HEADER_LEN + frame_len;
        if buf.len() < total {
            return Ok(None);
        }
        let body = &buf[FRAME_HEADER_LEN..total];

        let frame = match frame_type {
            0x00 => Self::Data(DataFrame {
                stream_id,
                payload: body.to_vec(),
            }),
            0x01 => Self::Headers(HeadersFrame { stream_id }),
            0x03 => return Err(XpcError::HttpStreamReset.into()),
            0x04 => {
                // settings: a sequence of (u16 identifier, u32 value) entries
                let mut settings = Vec::new();
                let mut i = 0;
                while i + 6 <= body.len() {
                    let setting_type = u16::from_be_bytes([body[i], body[i + 1]]);
                    let value =
                        u32::from_be_bytes([body[i + 2], body[i + 3], body[i + 4], body[i + 5]]);
                    settings.push(match setting_type {
                        0x03 => Setting::MaxConcurrentStreams(value),
                        0x04 => Setting::InitialWindowSize(value),
                        _ => return Err(XpcError::UnknownHttpSetting(setting_type).into()),
                    });
                    i += 6;
                }
                Self::Settings(SettingsFrame {
                    settings,
                    stream_id,
                    flags,
                })
            }
            0x07 => {
                let msg = if body.len() < 8 {
                    "<MISSING>".to_string()
                } else {
                    String::from_utf8_lossy(&body[8..]).to_string()
                };
                return Err(XpcError::HttpGoAway(msg).into());
            }
            0x08 => {
                if body.len() != 4 {
                    return Err(IdeviceError::UnexpectedResponse(
                        "HTTP/2 window update frame body was not 4 bytes".into(),
                    ));
                }
                let window = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                Self::WindowUpdate(WindowUpdateFrame {
                    increment_size: window,
                    stream_id,
                })
            }
            _ => return Err(XpcError::UnknownFrame(frame_type).into()),
        };

        Ok(Some((frame, total)))
    }
}

#[derive(Debug, Clone)]
pub struct SettingsFrame {
    pub settings: Vec<Setting>,
    pub stream_id: u32,
    pub flags: u8,
}

#[derive(Debug, Clone)]
pub enum Setting {
    MaxConcurrentStreams(u32),
    InitialWindowSize(u32),
}

impl Setting {
    fn serialize(&self) -> Vec<u8> {
        match self {
            Setting::MaxConcurrentStreams(m) => {
                let mut res = vec![0x00, 0x03];
                res.extend(m.to_be_bytes());
                res
            }
            Setting::InitialWindowSize(s) => {
                let mut res = vec![0x00, 0x04];
                res.extend(s.to_be_bytes());
                res
            }
        }
    }
}

impl HttpFrame for SettingsFrame {
    fn serialize(&self) -> Vec<u8> {
        let settings = self
            .settings
            .iter()
            .map(|x| x.serialize())
            .collect::<Vec<Vec<u8>>>()
            .concat();
        let settings_len = (settings.len() as u32).to_be_bytes();
        let mut res = vec![
            settings_len[1],
            settings_len[2],
            settings_len[3],
            0x04,
            self.flags,
        ];
        res.extend(self.stream_id.to_be_bytes());
        res.extend(settings);
        res
    }
}

#[derive(Debug, Clone)]
pub struct WindowUpdateFrame {
    pub increment_size: u32,
    pub stream_id: u32,
}

impl HttpFrame for WindowUpdateFrame {
    fn serialize(&self) -> Vec<u8> {
        let mut res = vec![0x00, 0x00, 0x04, 0x08, 0x00]; // size, frame ID, flags
        res.extend(self.stream_id.to_be_bytes());
        res.extend(self.increment_size.to_be_bytes());
        res
    }
}

#[derive(Debug, Clone)]
/// We don't actually care about this frame according to spec. This is just to open new channels.
pub struct HeadersFrame {
    pub stream_id: u32,
}

impl HttpFrame for HeadersFrame {
    fn serialize(&self) -> Vec<u8> {
        let mut res = vec![0x00, 0x00, 0x00, 0x01, 0x04];
        res.extend(self.stream_id.to_be_bytes());
        res
    }
}

#[derive(Debug, Clone)]
pub struct DataFrame {
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

impl HttpFrame for DataFrame {
    fn serialize(&self) -> Vec<u8> {
        let mut res = (self.payload.len() as u32).to_be_bytes().to_vec();
        res.remove(0); // only 3 significant bytes
        res.extend([0x00, 0x00]); // frame type, flags
        res.extend(self.stream_id.to_be_bytes());
        res.extend(self.payload.clone());
        res
    }
}
