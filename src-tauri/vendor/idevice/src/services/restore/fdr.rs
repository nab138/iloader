//! FDR (Firmware Diagnostic Relay) trust channel
//!
//! During restore the device opens a trust channel used to proxy TLS connections
//! (e.g. to Apple's signing/trust servers) through the host, plus keep-alive
//! pings. FDR uses little-endian framing: a bare `u16` message tag, and
//! length-prefixed binary plists (4-byte little-endian length).
//!
//! FDR runs concurrently with the main restore loop on its own connection(s); a
//! caller spawns [`run_fdr_listener`] as a background task.

use std::{future::Future, pin::Pin, sync::Arc};

use plist::Value;
use tracing::{debug, warn};

use crate::{Idevice, IdeviceError};

/// The FDR control port.
pub const FDR_CTRL_PORT: u16 = 0x43A;

const CTRL_CMD: &[u8] = b"BeginCtrl\0";
const HELLO_CMD: &[u8] = b"HelloConn\0";
const CTRL_PROTO_VERSION: i64 = 2;

const FDR_SYNC_MSG: u16 = 0x1;
const FDR_PROXY_MSG: u16 = 0x105;
const FDR_PLIST_MSG: u16 = 0xBBAA;
const CHUNK_SIZE: u32 = 1 << 20;

/// Opens new connections to device ports for FDR
pub trait FdrConnector: Send + Sync {
    /// Connects to `port` on the restore-mode device.
    fn connect_device_port(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>>;
}

#[derive(Debug)]
pub struct FdrClient {
    idevice: Idevice,
}

impl FdrClient {
    /// Wraps a freshly-connected FDR socket.
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Performs the control handshake, returning the `ConnPort` for conn channels.
    pub async fn ctrl_handshake(&mut self) -> Result<u16, IdeviceError> {
        self.idevice.send_raw(CTRL_CMD).await?;
        let req = crate::plist!({
            "Command": Value::Data(CTRL_CMD.to_vec()),
            "CtrlProtoVersion": CTRL_PROTO_VERSION,
        });
        let resp = self.send_recv_plist(&req).await?;
        resp.get("ConnPort")
            .and_then(Value::as_unsigned_integer)
            .map(|p| p as u16)
            .ok_or_else(|| {
                IdeviceError::UnexpectedResponse("FDR ctrl reply missing ConnPort".into())
            })
    }

    /// Performs the conn (sync) handshake.
    pub async fn sync_handshake(&mut self) -> Result<(), IdeviceError> {
        self.idevice.send_raw(HELLO_CMD).await?;
        let reply = self.recv_plist().await?;
        match reply.get("Command").and_then(Value::as_string) {
            Some("HelloConn") => Ok(()),
            other => Err(IdeviceError::UnexpectedResponse(format!(
                "expected HelloConn, got {other:?}"
            ))),
        }
    }

    async fn recv_plist(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let len = self.idevice.read_raw(4).await?;
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let body = self.idevice.read_raw(len).await?;
        Ok(plist::from_bytes(&body)?)
    }

    async fn send_plist(&mut self, value: &Value) -> Result<(), IdeviceError> {
        let mut body = Vec::new();
        value
            .to_writer_binary(&mut body)
            .map_err(IdeviceError::Plist)?;
        self.idevice
            .send_raw(&(body.len() as u32).to_le_bytes())
            .await?;
        self.idevice.send_raw(&body).await
    }

    async fn send_recv_plist(&mut self, value: &Value) -> Result<plist::Dictionary, IdeviceError> {
        self.send_plist(value).await?;
        self.recv_plist().await
    }

    /// Reads the next little-endian `u16` message tag.
    async fn read_message_tag(&mut self) -> Result<u16, IdeviceError> {
        let b = self.idevice.read_raw(2).await?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    async fn handle_plist(&mut self) -> Result<(), IdeviceError> {
        let d = self.recv_plist().await?;
        match d.get("Command").and_then(Value::as_string) {
            Some("Ping") => {
                let _ = self
                    .send_recv_plist(&crate::plist!({ "Pong": true }))
                    .await?;
            }
            other => warn!("FDR: unknown plist command {other:?}"),
        }
        Ok(())
    }

    async fn handle_proxy(mut self) -> Result<(), IdeviceError> {
        let buf = self.idevice.read_any(CHUNK_SIZE).await?;
        debug!("FDR proxy command with {} bytes", buf.len());

        // Acknowledge the request (u16 = 5) and echo the payload back.
        self.idevice.send_raw(&5u16.to_le_bytes()).await?;
        if buf.len() < 3 {
            debug!("FDR proxy command too short");
            return Ok(());
        }
        self.idevice.send_raw(&buf).await?;

        // SOCKS-ish connect request: [0x00, 0x03, hostlen, host..., portBE(2)].
        if buf[0] != 0 || buf[1] != 3 {
            return Ok(());
        }
        let hostlen = buf[2] as usize;
        if 3 + hostlen + 2 > buf.len() {
            warn!("FDR proxy connect request truncated");
            return Ok(());
        }
        let host = String::from_utf8_lossy(&buf[3..3 + hostlen]).to_string();
        let port = u16::from_be_bytes([buf[buf.len() - 2], buf[buf.len() - 1]]);
        debug!("FDR proxy connect to {host}:{port}");

        use tokio::io::AsyncWriteExt;

        let host_stream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
        let device = self
            .idevice
            .get_socket()
            .ok_or(IdeviceError::NoEstablishedConnection)?;

        // Bidirectionally bridge the device stream and the outbound TCP stream
        // until either side closes.
        let (mut dr, mut dw) = tokio::io::split(device);
        let (mut hr, mut hw) = tokio::io::split(host_stream);
        let s2h = async {
            let _ = tokio::io::copy(&mut dr, &mut hw).await;
            let _ = hw.shutdown().await;
        };
        let h2s = async {
            let _ = tokio::io::copy(&mut hr, &mut dw).await;
            let _ = dw.shutdown().await;
        };
        tokio::select! {
            _ = s2h => {},
            _ = h2s => {},
        }
        Ok(())
    }
}

/// Runs an FDR listener loop until the connection closes.
///
/// On a `Sync` message a new conn-channel listener is spawned against
/// `conn_port`; `Proxy` messages take over the connection to bridge traffic;
/// `Plist` messages answer pings.
///
/// Spawn this as a background task alongside the restore loop. Returns a boxed,
/// `Send` future so the listener can spawn further conn-channel listeners
/// recursively.
pub fn run_fdr_listener(
    mut client: FdrClient,
    connector: Arc<dyn FdrConnector>,
    conn_port: u16,
) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send>> {
    Box::pin(async move {
        loop {
            let tag = client.read_message_tag().await?;
            match tag {
                FDR_SYNC_MSG => {
                    // Consume the 2-byte sync payload and spin up a new conn channel.
                    let _ = client.idevice.read_raw(2).await?;
                    let connector = connector.clone();
                    tokio::spawn(async move {
                        match connector.connect_device_port(conn_port).await {
                            Ok(idevice) => {
                                let mut conn = FdrClient::new(idevice);
                                if let Err(e) = conn.sync_handshake().await {
                                    warn!("FDR conn handshake failed: {e}");
                                    return;
                                }
                                if let Err(e) = run_fdr_listener(conn, connector, conn_port).await {
                                    debug!("FDR conn listener ended: {e}");
                                }
                            }
                            Err(e) => warn!("FDR conn connect failed: {e}"),
                        }
                    });
                }
                FDR_PROXY_MSG => {
                    // The proxy takes over this connection; the listener ends here.
                    return client.handle_proxy().await;
                }
                FDR_PLIST_MSG => client.handle_plist().await?,
                other => warn!("FDR: ignoring message tag {other:#x}"),
            }
        }
    })
}
