mod state;

use std::sync::Arc;
use std::time::SystemTime;

use base64::prelude::*;
use plist_macro::plist;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use rootcause::option_ext::OptionExt;
use rootcause::prelude::*;
use serde::Deserialize;
use tokio::time::{Duration, timeout};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::SideloadError;
use crate::anisette::remote_v3::state::AnisetteState;
use crate::anisette::{AnisetteClientInfo, AnisetteData, AnisetteProvider};
use crate::auth::grandslam::GrandSlam;
use crate::util::plist::PlistDataExtract;
use crate::util::storage::{SideloadingStorage, new_storage};
use futures_util::{SinkExt, StreamExt};

pub const DEFAULT_ANISETTE_V3_URL: &str = "https://ani.stikstore.app";

pub struct RemoteV3AnisetteProvider {
    pub state: Option<AnisetteState>,
    url: String,
    storage: Box<dyn SideloadingStorage>,
    serial_number: String,
    client_info: Option<AnisetteClientInfo>,
    client: reqwest::Client,
}

impl RemoteV3AnisetteProvider {
    /// Create a new RemoteV3AnisetteProvider with the given URL and config path
    ///
    /// # Arguments
    /// - `url`: The URL of the remote anisette service
    /// - `storage`: The storage backend for anisette data
    /// - `serial_number`: The serial number of the device
    pub fn new(
        url: &str,
        storage: Box<dyn SideloadingStorage>,
        serial_number: String,
    ) -> Result<Self, Report> {
        Ok(Self {
            state: None,
            url: url.to_string(),
            storage,
            serial_number,
            client_info: None,
            client: reqwest::ClientBuilder::new()
                .build()
                .context("Failed to build HTTP client")?,
        })
    }

    pub fn default() -> Result<Self, Report> {
        Self::new(
            DEFAULT_ANISETTE_V3_URL,
            Box::new(new_storage()),
            "0".to_string(),
        )
    }

    pub fn set_url(mut self, url: &str) -> RemoteV3AnisetteProvider {
        self.url = url.to_string();
        self
    }

    pub fn set_storage(mut self, storage: Box<dyn SideloadingStorage>) -> RemoteV3AnisetteProvider {
        self.storage = storage;
        self
    }

    pub fn set_serial_number(mut self, serial_number: String) -> RemoteV3AnisetteProvider {
        self.serial_number = serial_number;
        self
    }
}

#[async_trait::async_trait]
impl AnisetteProvider for RemoteV3AnisetteProvider {
    async fn get_anisette_data(&self) -> Result<AnisetteData, Report> {
        let state = self
            .state
            .as_ref()
            .ok_or(SideloadError::AnisetteNotProvisioned)?;
        let adi_pb = state
            .adi_pb
            .as_ref()
            .ok_or(SideloadError::AnisetteNotProvisioned)?;
        let client_info = self
            .client_info
            .as_ref()
            .ok_or(SideloadError::AnisetteNotProvisioned)?;

        let headers = self
            .client
            .post(format!("{}/v3/get_headers", self.url))
            .header(CONTENT_TYPE, "application/json")
            .body(
                serde_json::json!({
                "identifier": BASE64_STANDARD.encode(state.keychain_identifier),
                "adi_pb": BASE64_STANDARD.encode(adi_pb)
                })
                .to_string(),
            )
            .send()
            .await?
            .error_for_status()?
            .json::<AnisetteHeaders>()
            .await?;

        match headers {
            AnisetteHeaders::Headers {
                machine_id,
                one_time_password,
                routing_info,
            } => {
                let data = AnisetteData {
                    machine_id,
                    one_time_password,
                    routing_info,
                    _device_description: client_info.client_info.clone(),
                    device_unique_identifier: state.get_device_id(),
                    _local_user_id: hex::encode(state.get_md_lu()),
                    generated_at: SystemTime::now(),
                };

                Ok(data)
            }
            AnisetteHeaders::GetHeadersError { message } => {
                Err(report!("Failed to get anisette headers").attach(message))
            }
        }
    }

    async fn get_client_info(&mut self) -> Result<AnisetteClientInfo, Report> {
        match self.client_info {
            Some(ref info) => Ok(info.clone()),
            None => {
                let resp = self
                    .client
                    .get(format!("{}/v3/client_info", self.url))
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<AnisetteClientInfo>()
                    .await?;

                self.client_info = Some(resp.clone());
                Ok(resp)
            }
        }
    }

    fn needs_provisioning(&self) -> Result<bool, Report> {
        if let Some(state) = &self.state {
            Ok(!state.is_provisioned() || self.client_info.is_none())
        } else {
            Ok(true)
        }
    }

    async fn provision(&mut self, gs: Arc<GrandSlam>) -> Result<(), Report> {
        self.get_client_info().await?;
        self.get_state(gs).await?;
        Ok(())
    }
}

impl RemoteV3AnisetteProvider {
    async fn get_state(&mut self, gs: Arc<GrandSlam>) -> Result<&mut AnisetteState, Report> {
        if self.state.is_none() {
            if let Ok(Some(state)) = &self.storage.retrieve_data("anisette_state") {
                if let Ok(state) = plist::from_bytes(state) {
                    info!("Loaded existing anisette state");
                    self.state = Some(state);
                } else {
                    warn!("Failed to parse existing anisette state, starting fresh");
                    self.state = Some(AnisetteState::new());
                }
            } else {
                info!("No existing anisette state found");
                self.state = Some(AnisetteState::new());
            }
        }

        let state = self.state.as_mut().ok_or_report()?;
        if !state.is_provisioned() {
            info!("Provisioning required...");
            Self::provision(state, gs, &self.url)
                .await
                .context("Failed to provision")?;
        }
        let buf = Vec::new();
        let mut writer = std::io::BufWriter::new(buf);
        plist::to_writer_xml(&mut writer, &state)?;
        self.storage
            .store_data("anisette_state", &writer.into_inner()?)?;

        Ok(state)
    }

    async fn provisioning_headers(state: &AnisetteState) -> Result<HeaderMap, Report> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Apple-I-MD-LU",
            HeaderValue::from_str(&hex::encode(state.get_md_lu()))?,
        );
        // headers.insert(
        //     "X-Apple-I-Client-Time",
        //     HeaderValue::from_str(
        //         &Utc::now()
        //             .round_subsecs(0)
        //             .format("%+")
        //             .to_string()
        //             .replace("+00:00", "Z"),
        //     )?,
        // );
        // headers.insert("X-Apple-I-TimeZone", HeaderValue::from_static("UTC"));
        // headers.insert("X-Apple-Locale", HeaderValue::from_static("en_US"));
        headers.insert(
            "X-Mme-Device-Id",
            HeaderValue::from_str(&state.get_device_id())?,
        );

        Ok(headers)
    }
    async fn provision(
        state: &mut AnisetteState,
        gs: Arc<GrandSlam>,
        url: &str,
    ) -> Result<(), Report> {
        let start_provisioning = gs.get_url("midStartProvisioning")?;
        let end_provisioning = gs.get_url("midFinishProvisioning")?;

        let websocket_url = format!("{}/v3/provisioning_session", url)
            .replace("https://", "wss://")
            .replace("http://", "ws://");

        debug!("Starting provisioning at {}", websocket_url);
        let (mut ws_stream, _) = timeout(
            Duration::from_secs(30),
            tokio_tungstenite::connect_async(&websocket_url),
        )
        .await
        .map_err(|_| {
            report!("Timed out connecting to provisioning socket. Try a different anisette server.")
        })
        .context("Failed to connect to provisioning socket")?
        .context("Failed to connect to provisioning socket")?;

        debug!("Connected to provisioning socket");

        loop {
            let Some(msg) = ws_stream.next().await else {
                continue;
            };
            let msg = msg.context("Failed to read anisette provisioning socket message")?;
            if msg.is_close() {
                bail!("Anisette provisioning socket closed unexpectedly");
            }
            let msg = msg
                .into_text()
                .context("Failed to parse provisioning message")?;

            debug!("Received provisioning message: {}", msg);
            let provision_msg: ProvisioningMessage =
                serde_json::from_str(&msg).context("Unknown provisioning message")?;

            match provision_msg {
                ProvisioningMessage::GiveIdentifier => {
                    ws_stream
                        .send(Message::Text(
                            serde_json::json!({
                                "identifier": BASE64_STANDARD.encode(state.keychain_identifier),
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .context("Failed to send identifier")?;
                }
                ProvisioningMessage::GiveStartProvisioningData => {
                    let body = plist!(dict {
                        "Header": {},
                        "Request": {}
                    });

                    let response = gs
                        .plist_request(
                            &start_provisioning,
                            &body,
                            Some(Self::provisioning_headers(state).await?),
                        )
                        .await
                        .context("Failed to send start provisioning request")?;

                    let spim = response
                        .get_str("spim")
                        .context("Start provisioning response missing spim")?;

                    ws_stream
                        .send(Message::Text(
                            serde_json::json!({
                                "spim": spim,
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .context("Failed to send start provisioning data")?;
                }
                ProvisioningMessage::GiveEndProvisioningData { cpim } => {
                    let body = plist!(dict {
                        "Header": {},
                        "Request": {
                            "cpim": cpim,
                        }
                    });

                    let response = gs
                        .plist_request(
                            &end_provisioning,
                            &body,
                            Some(Self::provisioning_headers(state).await?),
                        )
                        .await
                        .context("Failed to send end provisioning request")?;

                    ws_stream
                        .send(Message::Text(
                            serde_json::json!({
                                "ptm": response
                                    .get_str("ptm")
                                    .context("End provisioning response missing ptm")?,
                                "tk": response
                                    .get_str("tk")
                                    .context("End provisioning response missing tk")?,
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .context("Failed to send start provisioning data")?;
                }
                ProvisioningMessage::ProvisioningSuccess { adi_pb } => {
                    state.adi_pb = Some(BASE64_STANDARD.decode(adi_pb)?);
                    ws_stream.close(None).await?;
                    info!("Provisioning successful");
                    break;
                }
                ProvisioningMessage::Timeout => bail!("Anisette provisioning timed out"),
                ProvisioningMessage::InvalidIdentifier => {
                    bail!("Anisette provisioning failed: invalid identifier")
                }
                ProvisioningMessage::StartProvisioningError { message } => {
                    return Err(
                        report!("Anisette provisioning failed: start provisioning error")
                            .attach(message),
                    );
                }
                ProvisioningMessage::EndProvisioningError { message } => {
                    return Err(
                        report!("Anisette provisioning failed: end provisioning error")
                            .attach(message),
                    );
                }
            }
        }

        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "result")]
enum ProvisioningMessage {
    GiveIdentifier,
    GiveStartProvisioningData,
    GiveEndProvisioningData { cpim: String },
    ProvisioningSuccess { adi_pb: String },
    Timeout,
    InvalidIdentifier,
    StartProvisioningError { message: String },
    EndProvisioningError { message: String },
}

#[derive(Deserialize)]
#[serde(tag = "result")]
enum AnisetteHeaders {
    GetHeadersError {
        message: String,
    },
    Headers {
        #[serde(rename = "X-Apple-I-MD-M")]
        machine_id: String,
        #[serde(rename = "X-Apple-I-MD")]
        one_time_password: String,
        #[serde(rename = "X-Apple-I-MD-RINFO")]
        routing_info: String,
    },
}
