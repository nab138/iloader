use std::{future::Future, pin::Pin};

use plist::Value;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::debug;

use crate::{
    Idevice, IdeviceError,
    services::restore::{
        RestoreError,
        data_request::{PROGRESS_STRIDE, emit_transfer, is_cancelled},
        state_machine::{RestoreCancel, RestoreProgressSender},
    },
};

const ASR_VERSION: i64 = 1;
const ASR_STREAM_ID: i64 = 1;
const ASR_FEC_SLICE_STRIDE: i64 = 40;
const ASR_PACKETS_PER_FEC: i64 = 25;
const ASR_PAYLOAD_PACKET_SIZE: i64 = 1450;
const ASR_PAYLOAD_CHUNK_SIZE: u64 = 0x20000;

/// A seekable, sized source for the filesystem image.
pub trait FilesystemImage: Send {
    /// Total size of the image in bytes.
    fn size(&mut self) -> Pin<Box<dyn Future<Output = Result<u64, IdeviceError>> + Send + '_>>;
    /// Reads up to `len` bytes starting at `offset` (short only at end of image).
    fn read_at(
        &mut self,
        offset: u64,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, IdeviceError>> + Send + '_>>;
}

/// Blanket implementation for any seekable async reader
impl<T> FilesystemImage for T
where
    T: AsyncReadExt + AsyncSeekExt + Unpin + Send,
{
    fn size(&mut self) -> Pin<Box<dyn Future<Output = Result<u64, IdeviceError>> + Send + '_>> {
        Box::pin(async move { Ok(self.seek(std::io::SeekFrom::End(0)).await?) })
    }

    fn read_at(
        &mut self,
        offset: u64,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, IdeviceError>> + Send + '_>> {
        Box::pin(async move {
            self.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut buf = vec![0u8; len];
            let mut read = 0;
            while read < len {
                let n = self.read(&mut buf[read..]).await?;
                if n == 0 {
                    break;
                }
                read += n;
            }
            buf.truncate(read);
            Ok(buf)
        })
    }
}

/// A client speaking the ASR protocol over a data-port connection.
#[derive(Debug)]
pub struct AsrClient {
    idevice: Idevice,
    checksum_chunks: bool,
}

impl AsrClient {
    pub const DEFAULT_PORT: u16 = 12345;

    /// Wraps a connection to the ASR port and consumes the initial `Initiate`.
    pub async fn connect(idevice: Idevice) -> Result<Self, IdeviceError> {
        let mut client = Self {
            idevice,
            checksum_chunks: false,
        };
        let init = client.recv_plist().await?;
        match init.get("Command").and_then(Value::as_string) {
            Some("Initiate") => {}
            other => {
                return Err(IdeviceError::UnexpectedResponse(format!(
                    "expected ASR Initiate, got {other:?}"
                )));
            }
        }
        client.checksum_chunks = init
            .get("Checksum Chunks")
            .and_then(Value::as_boolean)
            .unwrap_or(false);
        debug!("ASR checksum_chunks = {}", client.checksum_chunks);
        Ok(client)
    }

    /// Reads a raw XML plist terminated by `</plist>\n`.
    async fn recv_plist(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        const TERMINATOR: &[u8] = b"</plist>\n";
        let mut buf = Vec::new();
        loop {
            let b = self.idevice.read_raw(1).await?;
            if b.is_empty() {
                return Err(IdeviceError::UnexpectedResponse(
                    "ASR connection closed mid-plist".into(),
                ));
            }
            buf.push(b[0]);
            if buf.ends_with(TERMINATOR) {
                break;
            }
        }
        Ok(plist::from_bytes(&buf)?)
    }

    /// Sends an XML plist (no length prefix).
    ///
    /// ASR frames plists on the `</plist>\n` terminator, so a trailing newline is
    /// appended when the serializer omits it.
    async fn send_plist(&mut self, value: &Value) -> Result<(), IdeviceError> {
        let mut buf = Vec::new();
        value.to_writer_xml(&mut buf).map_err(IdeviceError::Plist)?;
        if !buf.ends_with(b"\n") {
            buf.push(b'\n');
        }
        self.idevice.send_raw(&buf).await
    }

    /// Runs the full validation + payload transfer for `image`.
    ///
    /// `progress` receives throttled [`RestoreProgressEvent::Transfer`] updates as
    /// the payload streams; `cancel`, when set, aborts the transfer per chunk with
    /// [`RestoreError::Cancelled`]. This is the longest phase of a restore, so it is
    /// where cancellation and byte-progress matter most.
    pub async fn send_filesystem(
        &mut self,
        image: &mut dyn FilesystemImage,
        progress: Option<RestoreProgressSender>,
        cancel: Option<RestoreCancel>,
    ) -> Result<(), IdeviceError> {
        self.perform_validation(image).await?;
        self.send_payload(image, progress.as_ref(), cancel.as_ref())
            .await
    }

    /// Sends the validation packet and services `OOBData` until `Payload`.
    async fn perform_validation(
        &mut self,
        image: &mut dyn FilesystemImage,
    ) -> Result<(), IdeviceError> {
        let length = image.size().await?;

        let mut payload_info = plist::Dictionary::new();
        payload_info.insert("Port".into(), 1.into());
        payload_info.insert("Size".into(), (length as i64).into());

        let mut packet_info = plist::Dictionary::new();
        if self.checksum_chunks {
            packet_info.insert(
                "Checksum Chunk Size".into(),
                (ASR_PAYLOAD_CHUNK_SIZE as i64).into(),
            );
        }
        packet_info.insert("FEC Slice Stride".into(), ASR_FEC_SLICE_STRIDE.into());
        packet_info.insert("Packet Payload Size".into(), ASR_PAYLOAD_PACKET_SIZE.into());
        packet_info.insert("Packets Per FEC".into(), ASR_PACKETS_PER_FEC.into());
        packet_info.insert("Payload".into(), Value::Dictionary(payload_info));
        packet_info.insert("Stream ID".into(), ASR_STREAM_ID.into());
        packet_info.insert("Version".into(), ASR_VERSION.into());

        self.send_plist(&Value::Dictionary(packet_info)).await?;

        loop {
            let packet = self.recv_plist().await?;
            match packet.get("Command").and_then(Value::as_string) {
                Some("Payload") => return Ok(()),
                Some("OOBData") => self.handle_oob(&packet, image).await?,
                other => {
                    return Err(IdeviceError::UnexpectedResponse(format!(
                        "unexpected ASR command during validation: {other:?}"
                    )));
                }
            }
        }
    }

    /// Answers an out-of-band data request with the requested image range.
    async fn handle_oob(
        &mut self,
        packet: &plist::Dictionary,
        image: &mut dyn FilesystemImage,
    ) -> Result<(), IdeviceError> {
        let offset = packet
            .get("OOB Offset")
            .and_then(Value::as_unsigned_integer)
            .ok_or_else(|| IdeviceError::UnexpectedResponse("OOBData missing OOB Offset".into()))?;
        let length = packet
            .get("OOB Length")
            .and_then(Value::as_unsigned_integer)
            .ok_or_else(|| IdeviceError::UnexpectedResponse("OOBData missing OOB Length".into()))?;

        let data = image.read_at(offset, length as usize).await?;
        if data.len() as u64 != length {
            return Err(IdeviceError::Restore(RestoreError::Asr(format!(
                "ASR OOB read short: wanted {length}, got {}",
                data.len()
            ))));
        }
        self.idevice.send_raw(&data).await
    }

    /// Streams the image in chunks, appending a per-chunk SHA-1 when requested.
    ///
    /// Checks `cancel` before each chunk (so a cancellation lands within one
    /// chunk's worth of transfer) and emits throttled byte progress into `progress`.
    async fn send_payload(
        &mut self,
        image: &mut dyn FilesystemImage,
        progress: Option<&RestoreProgressSender>,
        cancel: Option<&RestoreCancel>,
    ) -> Result<(), IdeviceError> {
        let length = image.size().await?;
        let mut offset = 0u64;
        let mut next_report = 0u64;
        while offset < length {
            if is_cancelled(cancel) {
                return Err(IdeviceError::Restore(RestoreError::Cancelled));
            }
            let want = ASR_PAYLOAD_CHUNK_SIZE.min(length - offset) as usize;
            let mut chunk = image.read_at(offset, want).await?;
            if chunk.is_empty() {
                break;
            }
            offset += chunk.len() as u64;

            if self.checksum_chunks {
                let digest = Sha1::digest(&chunk);
                chunk.extend_from_slice(&digest);
            }
            self.idevice.send_raw(&chunk).await?;

            if offset >= next_report {
                emit_transfer(progress, "filesystem", offset, Some(length));
                next_report = offset + PROGRESS_STRIDE;
            }
        }
        // A final 100% event so the consumer sees the transfer complete.
        emit_transfer(progress, "filesystem", offset, Some(length));
        Ok(())
    }
}
