use futures::{SinkExt, StreamExt};
use rootcause::prelude::*;

pub enum WsMessage {
    Text(String),
    Close,
}

pub struct AppWebSocket {
    #[cfg(not(feature = "wasm"))]
    inner: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    #[cfg(feature = "wasm")]
    inner: ws_stream_wasm::WsStream,
    #[cfg(feature = "wasm")]
    meta: ws_stream_wasm::WsMeta,
}

impl AppWebSocket {
    pub async fn connect(url: &str, proxy_url: Option<&str>) -> Result<Self, Report> {
        let target_url = if proxy_url.is_some() {
            let proxied = format!(
                "{}{}",
                proxy_url.unwrap(),
                urlencoding::encode(url)
            )
            .replace("https://", "wss://");
            proxied
        } else {
            url.to_string()
        };

        #[cfg(not(feature = "wasm"))]
        {
            use tokio::time::{Duration, timeout};

            let (stream, _) = timeout(
                Duration::from_secs(30),
                tokio_tungstenite::connect_async(&target_url),
            )
            .await?
            .context(
                "Timed out connecting to provisioning socket. Try a different anisette server.",
            )?;
            Ok(Self { inner: stream })
        }
        #[cfg(feature = "wasm")]
        {
            let (meta, stream) = ws_stream_wasm::WsMeta::connect(&target_url, None)
                .await
                .map_err(|e| report!("WS connect failed: {e:?}"))?;
            Ok(Self {
                meta,
                inner: stream,
            })
        }
    }

    pub async fn send_text(&mut self, text: String) -> Result<(), Report> {
        #[cfg(not(feature = "wasm"))]
        self.inner
            .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
            .await?;
        #[cfg(feature = "wasm")]
        self.inner
            .send(ws_stream_wasm::WsMessage::Text(text))
            .await
            .map_err(|e| report!("WS send failed: {e:?}"))?;
        Ok(())
    }

    pub async fn next(&mut self) -> Option<Result<WsMessage, Report>> {
        #[cfg(not(feature = "wasm"))]
        {
            let msg = self.inner.next().await?;
            Some(msg.map_err(Into::into).map(|m| match m {
                tokio_tungstenite::tungstenite::Message::Text(t) => WsMessage::Text(t.to_string()),
                tokio_tungstenite::tungstenite::Message::Close(_) => WsMessage::Close,
                _ => WsMessage::Close,
            }))
        }
        #[cfg(feature = "wasm")]
        {
            let msg = self.inner.next().await?;
            Some(Ok(match msg {
                ws_stream_wasm::WsMessage::Text(t) => WsMessage::Text(t),
                ws_stream_wasm::WsMessage::Binary(_) => WsMessage::Close,
            }))
        }
    }

    pub async fn close(&mut self) -> Result<(), Report> {
        #[cfg(not(feature = "wasm"))]
        self.inner.close(None).await?;
        #[cfg(feature = "wasm")]
        self.meta
            .close()
            .await
            .map_err(|e| report!("WS close failed: {e:?}"))?;
        Ok(())
    }
}
