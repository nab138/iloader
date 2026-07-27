// Jackson Coxson

use frame::HttpFrame;
use std::collections::{HashMap, VecDeque};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use crate::{IdeviceError, ReadWrite};

pub mod frame;
pub use frame::Setting;

const HTTP2_MAGIC: &[u8] = "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".as_bytes();

/// HTTP/2 default initial flow-control window for both the connection and each
/// stream (RFC 7540 §6.9.2). The peer can raise the per-stream default via a
/// SETTINGS `InitialWindowSize`.
const DEFAULT_WINDOW: i64 = 65535;

#[derive(Debug)]
pub struct Http2Client<R: ReadWrite> {
    inner: R,
    cache: HashMap<u32, VecDeque<Vec<u8>>>,
    /// How many payload octets we may still send on the connection as a whole
    /// before the peer must replenish it with a connection-level WINDOW_UPDATE.
    conn_send_window: i64,
    /// Per-stream remaining send window. Lazily seeded to `peer_initial_window`.
    stream_send_windows: HashMap<u32, i64>,
    /// The peer's current SETTINGS `InitialWindowSize` — the window each *new*
    /// stream starts with.
    peer_initial_window: i64,
    /// Raw inbound bytes not yet parsed into a whole frame. Persisting this
    /// across reads keeps [`Self::pump`] (and therefore `recv_push`)
    /// cancellation-safe: a partially-received frame survives a dropped read.
    recv_buf: Vec<u8>,
}

impl<R: ReadWrite> Http2Client<R> {
    /// Writes the magic and inits the caches
    pub async fn new(mut inner: R) -> Result<Self, IdeviceError> {
        inner.write_all(HTTP2_MAGIC).await?;
        inner.flush().await?;
        Ok(Self {
            inner,
            cache: HashMap::new(),
            conn_send_window: DEFAULT_WINDOW,
            stream_send_windows: HashMap::new(),
            peer_initial_window: DEFAULT_WINDOW,
            recv_buf: Vec::new(),
        })
    }

    /// Read the next whole frame, buffering raw bytes in `recv_buf` until one is
    /// complete. Cancellation-safe: the single `read` is cancel-safe (no bytes
    /// lost if the future is dropped on `Pending`), and any bytes already
    /// buffered persist in `self` for the next call.
    async fn next_frame(&mut self) -> Result<frame::Frame, IdeviceError> {
        loop {
            if let Some((frame, consumed)) = frame::Frame::parse(&self.recv_buf)? {
                self.recv_buf.drain(..consumed);
                return Ok(frame);
            }
            let mut tmp = [0u8; 16384];
            let n = self.inner.read(&mut tmp).await?;
            if n == 0 {
                return Err(IdeviceError::UnexpectedResponse(
                    "HTTP/2 connection closed by peer".into(),
                ));
            }
            self.recv_buf.extend_from_slice(&tmp[..n]);
        }
    }

    pub async fn set_settings(
        &mut self,
        settings: Vec<frame::Setting>,
        stream_id: u32,
    ) -> Result<(), IdeviceError> {
        let frame = frame::SettingsFrame {
            settings,
            stream_id,
            flags: 0,
        }
        .serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn window_update(
        &mut self,
        increment_size: u32,
        stream_id: u32,
    ) -> Result<(), IdeviceError> {
        let frame = frame::WindowUpdateFrame {
            increment_size,
            stream_id,
        }
        .serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn open_stream(&mut self, stream_id: u32) -> Result<(), IdeviceError> {
        // Sometimes Apple is silly and sends data to a stream that isn't open
        self.cache.entry(stream_id).or_default();
        let frame = frame::HeadersFrame { stream_id }.serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn send(&mut self, payload: Vec<u8>, stream_id: u32) -> Result<(), IdeviceError> {
        const MAX_FRAME_SIZE: usize = 16384;
        let mut chunks = payload.chunks(MAX_FRAME_SIZE).peekable();
        // Always send at least one frame, even for an empty payload. An empty
        // DATA frame costs no flow-control window, so send it directly.
        if chunks.peek().is_none() {
            let frame = frame::DataFrame {
                stream_id,
                payload: Vec::new(),
            }
            .serialize();
            self.inner.write_all(&frame).await?;
            self.inner.flush().await?;
            return Ok(());
        }
        for chunk in chunks {
            let need = chunk.len() as i64;
            // Respect the peer's flow-control window: a DATA frame must not exceed
            // either the connection-level or the stream-level send window, or the
            // peer aborts the connection with a GOAWAY (FLOW_CONTROL_ERROR). When
            // either window is exhausted, pump inbound frames until the peer grants
            // more room with a WINDOW_UPDATE. (Matters for large payloads like
            // pasteboard images; small ones fit in the initial 64 KiB window.)
            while self.conn_send_window < need || self.stream_send_window(stream_id) < need {
                self.pump().await?;
            }
            let frame = frame::DataFrame {
                stream_id,
                payload: chunk.to_vec(),
            }
            .serialize();
            self.inner.write_all(&frame).await?;
            self.conn_send_window -= need;
            *self
                .stream_send_windows
                .get_mut(&stream_id)
                .expect("seeded by stream_send_window above") -= need;
        }
        self.inner.flush().await?;
        Ok(())
    }

    /// The remaining send window for `stream_id`, seeding it to the peer's current
    /// initial window size the first time we touch the stream.
    fn stream_send_window(&mut self, stream_id: u32) -> i64 {
        *self
            .stream_send_windows
            .entry(stream_id)
            .or_insert(self.peer_initial_window)
    }

    pub async fn read(&mut self, stream_id: u32) -> Result<Vec<u8>, IdeviceError> {
        self.cache.entry(stream_id).or_default();
        loop {
            // Return any frame already buffered for this stream.
            if let Some(d) = self.cache.get_mut(&stream_id).and_then(|c| c.pop_front()) {
                return Ok(d);
            }
            self.pump().await?;
        }
    }

    /// Read and handle a single inbound frame: ack SETTINGS (applying any
    /// `InitialWindowSize` change), apply WINDOW_UPDATEs to our send windows,
    /// replenish the peer's receive window for inbound DATA and buffer that DATA
    /// by stream. GOAWAY / RST_STREAM surface as errors via [`frame::Frame::next`].
    async fn pump(&mut self) -> Result<(), IdeviceError> {
        let frame = self.next_frame().await?;
        match frame {
            frame::Frame::Settings(settings_frame) if settings_frame.flags != 1 => {
                // Adjust every existing stream's send window by the delta in the
                // new InitialWindowSize (RFC 7540 §6.9.2), then ack.
                for setting in &settings_frame.settings {
                    if let frame::Setting::InitialWindowSize(new) = setting {
                        let delta = *new as i64 - self.peer_initial_window;
                        self.peer_initial_window = *new as i64;
                        for w in self.stream_send_windows.values_mut() {
                            *w += delta;
                        }
                    }
                }
                let ack = frame::SettingsFrame {
                    settings: Vec::new(),
                    stream_id: settings_frame.stream_id,
                    flags: 1,
                }
                .serialize();
                self.inner.write_all(&ack).await?;
                self.inner.flush().await?;
            }
            frame::Frame::WindowUpdate(w) => {
                if w.stream_id == 0 {
                    self.conn_send_window += w.increment_size as i64;
                } else {
                    let initial = self.peer_initial_window;
                    *self
                        .stream_send_windows
                        .entry(w.stream_id)
                        .or_insert(initial) += w.increment_size as i64;
                }
            }
            frame::Frame::Data(data_frame) => {
                debug!(
                    "Got data frame for {} with {} bytes",
                    data_frame.stream_id,
                    data_frame.payload.len()
                );

                let len = data_frame.payload.len() as u32;
                let stream_id = data_frame.stream_id;
                // Cache the payload BEFORE any await so a cancelled pump (the poll
                // tick interrupting `recv_push`) can never drop it.
                self.cache
                    .entry(stream_id)
                    .or_insert_with(|| {
                        // Apple sometimes sends data before the stream is "open".
                        warn!("Received message for stream ID {stream_id} not in cache");
                        VecDeque::new()
                    })
                    .push_back(data_frame.payload);
                if len > 0 {
                    // Replenish the peer's view of our receive window so it keeps
                    // sending (e.g. the rest of a large pasteboard image). Queue
                    // both WINDOW_UPDATE frames, then flush once: write_all on the
                    // tunnel stream queues a whole frame without suspending, so a
                    // cancellation can only land on the flush — by which point both
                    // frames are already queued (never a torn or dropped update).
                    let conn = frame::WindowUpdateFrame {
                        increment_size: len,
                        stream_id: 0,
                    }
                    .serialize();
                    let stream = frame::WindowUpdateFrame {
                        increment_size: len,
                        stream_id,
                    }
                    .serialize();
                    self.inner.write_all(&conn).await?;
                    self.inner.write_all(&stream).await?;
                    self.inner.flush().await?;
                }
            }
            _ => {
                // SETTINGS ack / HEADERS — nothing to do.
            }
        }
        Ok(())
    }
}
