// Jackson Coxson

use async_stream::try_stream;
use futures::Stream;
use http2::Setting;
use tracing::debug;

use crate::{CdTunnelError, IdeviceError, ReadWrite, xpc};

pub mod errors;
mod format;
mod http2;
pub mod xpc_macro;

use format::XPCFlag;
pub use format::{Dictionary, XPCMessage, XPCObject};

const ROOT_CHANNEL: u32 = 1;
const REPLY_CHANNEL: u32 = 3;

/// Fixed XPC message-wrapper header: magic + flags + body length + message id.
const XPC_WRAPPER_LEN: usize = 24;

#[derive(Debug)]
pub struct RemoteXpcClient<R: ReadWrite> {
    h2_client: http2::Http2Client<R>,
    root_id: u64,
    // reply_id: u64 // maybe not used?
    /// Per-channel bytes accumulated toward the next whole XPC message. Persisted
    /// across `recv_from_channel` calls so a partially-received message survives a
    /// cancelled read — required for `recv_push` to be safe in a `select!`.
    partial: std::collections::HashMap<u32, Vec<u8>>,
}

impl<R: ReadWrite> RemoteXpcClient<R> {
    pub async fn new(socket: R) -> Result<Self, IdeviceError> {
        Ok(Self {
            h2_client: http2::Http2Client::new(socket).await?,
            root_id: 1,
            partial: std::collections::HashMap::new(),
        })
    }

    pub async fn do_handshake(&mut self) -> Result<(), IdeviceError> {
        self.h2_client
            .set_settings(
                vec![
                    Setting::MaxConcurrentStreams(100),
                    Setting::InitialWindowSize(1048576),
                ],
                0,
            )
            .await?;
        self.h2_client.window_update(983041, 0).await?;
        self.h2_client.open_stream(1).await?; // root channel

        debug!("Sending empty dictionary");
        self.send_root(XPCMessage::new(
            Some(XPCFlag::AlwaysSet),
            Some(XPCObject::Dictionary(Default::default())),
            None,
        ))
        .await?;

        debug!("Opening reply stream");
        self.h2_client.open_stream(REPLY_CHANNEL).await?;
        self.send_reply(XPCMessage::new(
            Some(XPCFlag::InitHandshake | XPCFlag::AlwaysSet),
            None,
            None,
        ))
        .await?;

        debug!("Sending weird flags");
        self.send_root(XPCMessage::new(Some(XPCFlag::Custom(0x201)), None, None))
            .await?;

        Ok(())
    }

    /// Announce ourselves to the device's `remoted` as a modern (non-legacy)
    /// RemoteXPC peer.
    ///
    /// Send this only on the RSD/remoted control connection
    pub async fn send_device_handshake(&mut self) -> Result<(), IdeviceError> {
        const REMOTE_XPC_VERSION_FLAGS: u64 = 0x0100_0000_0000_0006;

        let msg = xpc!({
            "MessageType": "Handshake",
            "MessagingProtocolVersion": 7u64,
            "UUID": uuid::Uuid::new_v4(),
            "Properties": {
                "RemoteXPCVersionFlags": REMOTE_XPC_VERSION_FLAGS,
                "SensitivePropertiesVisible": true,
            },
            "Services": XPCObject::Dictionary(Dictionary::new())
        });

        self.send_object(msg, false).await
    }

    pub async fn recv(&mut self) -> Result<plist::Value, IdeviceError> {
        self.recv_from_channel(REPLY_CHANNEL).await
    }

    pub async fn recv_root(&mut self) -> Result<plist::Value, IdeviceError> {
        self.recv_from_channel(ROOT_CHANNEL).await
    }

    async fn recv_from_channel(&mut self, channel: u32) -> Result<plist::Value, IdeviceError> {
        loop {
            // Try to decode a whole message from what's already buffered before
            // reading more, so a message split across earlier reads completes.
            // Scope the buffer borrow so it's released before the await below.
            let decoded = {
                let buf = self.partial.entry(channel).or_default();
                match XPCMessage::decode(buf) {
                    Ok(msg) => {
                        // A complete wrapper consumes 24 + body_len bytes; drain
                        // exactly that, preserving any bytes of the next message.
                        let consumed = (XPC_WRAPPER_LEN + xpc_body_len(buf)).min(buf.len());
                        buf.drain(..consumed);
                        Some(msg)
                    }
                    // Not enough bytes yet: fall through to read another frame.
                    Err(IdeviceError::CdTunnel(CdTunnelError::SizeMismatch))
                    | Err(IdeviceError::NotEnoughBytes(..)) => None,
                    Err(e) => return Err(e),
                }
            };

            match decoded {
                Some(msg) => match msg.message {
                    // Skip empty-dictionary keepalives and bodyless frames.
                    Some(inner) => {
                        if let Some(d) = inner.as_dictionary()
                            && d.is_empty()
                        {
                            continue;
                        }
                        return Ok(inner.to_plist());
                    }
                    None => continue,
                },
                None => {
                    let chunk = self.h2_client.read(channel).await?;
                    self.partial.entry(channel).or_default().extend(chunk);
                }
            }
        }
    }

    pub async fn send_object(
        &mut self,
        msg: impl Into<XPCObject>,
        expect_reply: bool,
    ) -> Result<(), IdeviceError> {
        let msg: XPCObject = msg.into();

        let mut flag = XPCFlag::DataFlag | XPCFlag::AlwaysSet;
        if expect_reply {
            flag |= XPCFlag::WantingReply;
        }

        let msg = XPCMessage::new(Some(flag), Some(msg), Some(self.root_id));
        self.send_root(msg).await?;

        Ok(())
    }

    async fn send_root(&mut self, msg: XPCMessage) -> Result<(), IdeviceError> {
        self.h2_client
            .send(msg.encode(self.root_id)?, ROOT_CHANNEL)
            .await?;
        Ok(())
    }

    async fn send_reply(&mut self, msg: XPCMessage) -> Result<(), IdeviceError> {
        self.h2_client
            .send(msg.encode(self.root_id)?, REPLY_CHANNEL)
            .await?;
        Ok(())
    }

    pub fn iter_file_chunks<'a>(
        &'a mut self,
        total_size: usize,
        file_idx: u32,
    ) -> impl Stream<Item = Result<Vec<u8>, IdeviceError>> + 'a {
        let stream_id = (file_idx + 1) * 2;

        try_stream! {
            fn strip_xpc_wrapper_prefix(buf: &[u8]) -> (&[u8], bool) {
                // Returns (data_after_wrapper, stripped_anything)
                const MAGIC: u32 = 0x29b00b92;

                if buf.len() < 24 {
                    return (buf, false);
                }

                let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                if magic != MAGIC {
                    return (buf, false);
                }

                // flags at [4..8] – not needed to compute size
                let body_len = u64::from_le_bytes([
                    buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
                ]) as usize;

                let wrapper_len = 24 + body_len;
                if buf.len() < wrapper_len {
                    // Incomplete wrapper (shouldn’t happen with your read API), keep as-is.
                    return (buf, false);
                }

                (&buf[wrapper_len..], true)
            }
            self.open_file_stream_for_response(stream_id).await?;

            let mut got = 0usize;
            while got < total_size {
                let bytes = self.h2_client.read(stream_id).await?;
                let (after, stripped) = strip_xpc_wrapper_prefix(&bytes);
                if stripped && after.is_empty() {
                    continue; // pure control wrapper, don't count
                }

                let data = if stripped { after.to_vec() } else { bytes };

                if data.is_empty() {
                    continue;
                }

                got += data.len();
                yield data;
            }
        }
    }

    pub async fn open_file_stream_for_response(
        &mut self,
        stream_id: u32,
    ) -> Result<(), IdeviceError> {
        // 1) Open the HTTP/2 stream
        self.h2_client.open_stream(stream_id).await?;

        // 2) Send an empty XPC wrapper on that same stream with FILE_TX_STREAM_RESPONSE
        let flags = XPCFlag::AlwaysSet | XPCFlag::FileTxStreamResponse;

        let msg = XPCMessage::new(Some(flags), None, Some(0));

        // IMPORTANT: send on `stream_id`, not ROOT/REPLY
        let bytes = msg.encode(0)?;
        self.h2_client.send(bytes, stream_id).await?;
        Ok(())
    }
}

/// The XPC message body length from a decoded-OK wrapper: the little-endian u64
/// at bytes 8..16. `buf` is known to hold a full wrapper (≥ 24 bytes) here.
fn xpc_body_len(buf: &[u8]) -> usize {
    u64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]) as usize
}
