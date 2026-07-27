//! Restore-mode `restored` client
//!
//! Once a device boots the restore ramdisk it exposes `com.apple.mobile.restored`
//! on port 62078, spoken with the same framing as lockdown (4-byte big-endian
//! length prefix + XML plist). This client wraps an [`crate::Idevice`] connected
//! to that port and drives the request/response and streaming message protocol.

use plist::Value;
use tracing::debug;

use crate::{Idevice, IdeviceError};

/// The label sent with `restored` requests.
const DEFAULT_LABEL: &str = "idevice";

/// A client for the restore-mode `restored` service.
#[derive(Debug)]
pub struct RestoredClient {
    /// The underlying connection to port 62078 of a restore-mode device.
    pub idevice: Idevice,
    /// The `RestoreProtocolVersion` reported by the device (re-sent in `StartRestore`).
    pub protocol_version: Value,
    /// The label sent with each request.
    pub label: String,
    /// The usbmux `device_id` this client was found on, when it was discovered
    /// over usbmux (via [`connect_by_ecid`](Self::connect_by_ecid)). Data-port and
    /// FDR connectors should reuse this so their connections target the *same*
    /// physical device rather than whichever USB device usbmux lists first.
    pub device_id: Option<u32>,
}

impl RestoredClient {
    /// The restore-mode service port (shared with lockdown).
    pub const SERVICE_PORT: u16 = 62078;

    /// Wraps an [`Idevice`] already connected to port 62078 and performs the
    /// `QueryType` handshake, verifying the peer is `com.apple.mobile.restored`.
    ///
    /// # Errors
    /// Returns [`IdeviceError::UnexpectedResponse`] if the peer is not the
    /// restored service.
    pub async fn connect(idevice: Idevice) -> Result<Self, IdeviceError> {
        let mut client = Self {
            idevice,
            protocol_version: Value::Integer(0.into()),
            label: DEFAULT_LABEL.to_string(),
            device_id: None,
        };
        client.query_type().await?;
        Ok(client)
    }

    /// Sends `QueryType`, validates the service type and records the protocol version.
    async fn query_type(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Request": "QueryType",
            "Label": self.label.clone(),
        });
        self.idevice.send_plist(req).await?;
        let res = self.idevice.read_plist().await?;

        match res.get("Type").and_then(Value::as_string) {
            Some("com.apple.mobile.restored") => {}
            other => {
                return Err(IdeviceError::UnexpectedResponse(format!(
                    "expected com.apple.mobile.restored, got {other:?}"
                )));
            }
        }
        if let Some(v) = res.get("RestoreProtocolVersion") {
            self.protocol_version = v.clone();
        }
        debug!("restored protocol version: {:?}", self.protocol_version);
        Ok(())
    }

    /// Finds a restore-mode device by ECID and connects to `restored`.
    #[cfg(feature = "usbmuxd")]
    pub async fn connect_by_ecid(
        addr: &crate::usbmuxd::UsbmuxdAddr,
        ecid: u64,
        label: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, IdeviceError> {
        use crate::usbmuxd::Connection;

        let deadline = crate::time::Instant::now() + timeout;
        loop {
            let devices = match addr.connect(1).await {
                Ok(mut m) => m.get_devices().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            };

            for device in devices {
                if device.connection_type != Connection::Usb {
                    continue;
                }
                // A fresh mux connection per attempt (connect_to_device consumes it).
                let mux = match addr.connect(1).await {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let idevice = match mux
                    .connect_to_device(device.device_id, Self::SERVICE_PORT, label)
                    .await
                {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                let mut client = match Self::connect(idevice).await {
                    Ok(c) => c,
                    Err(_) => continue, // not a restored device
                };
                if let Ok(found) = client.ecid().await
                    && found == ecid
                {
                    // Remember which device this was so data-port / FDR connectors
                    // can target the same one rather than "first USB device".
                    client.device_id = Some(device.device_id);
                    return Ok(client);
                }
            }

            if crate::time::Instant::now() >= deadline {
                return Err(IdeviceError::DeviceNotFound);
            }
            crate::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    /// Queries a device value by key (`QueryValue`), returning the value stored
    /// under that key in the response.
    ///
    /// # Errors
    /// Returns [`IdeviceError::UnexpectedResponse`] if the response omits the key.
    pub async fn query_value(&mut self, key: &str) -> Result<Value, IdeviceError> {
        let req = crate::plist!({
            "Request": "QueryValue",
            "Label": self.label.clone(),
            "QueryKey": key,
        });
        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;
        res.remove(key).ok_or_else(|| {
            IdeviceError::UnexpectedResponse(format!("QueryValue response missing `{key}`"))
        })
    }

    /// Returns the device's `HardwareInfo` dictionary.
    pub async fn hardware_info(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        match self.query_value("HardwareInfo").await? {
            Value::Dictionary(d) => Ok(d),
            _ => Err(IdeviceError::UnexpectedResponse(
                "HardwareInfo is not a dictionary".into(),
            )),
        }
    }

    /// Returns the device's ECID (masked to 64 bits), read from `HardwareInfo`.
    pub async fn ecid(&mut self) -> Result<u64, IdeviceError> {
        let hw = self.hardware_info().await?;
        hw.get("UniqueChipID")
            .and_then(Value::as_unsigned_integer)
            .ok_or_else(|| {
                IdeviceError::UnexpectedResponse("HardwareInfo missing UniqueChipID".into())
            })
    }

    /// Begins the restore by sending `StartRestore` with the given options.
    ///
    /// This is a fire-and-forget request; the device subsequently drives the
    /// process by sending messages that the [state machine](super::state_machine)
    /// dispatches.
    pub async fn start_restore(&mut self, options: plist::Dictionary) -> Result<(), IdeviceError> {
        let mut req = plist::Dictionary::new();
        req.insert("Request".into(), "StartRestore".into());
        req.insert("Label".into(), self.label.clone().into());
        req.insert(
            "RestoreProtocolVersion".into(),
            self.protocol_version.clone(),
        );
        req.insert("RestoreOptions".into(), Value::Dictionary(options));
        self.idevice.send_plist(Value::Dictionary(req)).await
    }

    /// Requests a reboot, returning the response dictionary.
    ///
    /// From within Restore OS this reboots the device; because the restore entry
    /// leaves `auto-boot` set to `false`, iBoot then halts in recovery rather than
    /// attempting to boot a possibly half-written OS. This is the graceful bail-out
    /// used when a restore is cancelled.
    pub async fn reboot(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let req = crate::plist!({
            "Request": "Reboot",
            "Label": self.label.clone(),
        });
        self.idevice.send_plist(req).await?;
        self.idevice.read_plist().await
    }

    /// Sends a `Goodbye` request and best-effort reads its acknowledgement.
    ///
    /// Mirrors idevicerestore's teardown: the restore daemon is told the client is
    /// leaving so the connection closes cleanly instead of being dropped mid-stream.
    pub async fn goodbye(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let req = crate::plist!({
            "Request": "Goodbye",
            "Label": self.label.clone(),
        });
        self.idevice.send_plist(req).await?;
        self.idevice.read_plist().await
    }

    /// Sends a raw message to `restored`.
    pub async fn send(&mut self, message: Value) -> Result<(), IdeviceError> {
        self.idevice.send_plist(message).await
    }

    /// Receives the next message from `restored`.
    ///
    /// Uses the raw plist read rather than [`Idevice::read_plist`], because
    /// restore `StatusMsg`s carry a structured `Error` (a `CFErrorRef` dictionary)
    /// that the latter's error handling would choke on.
    pub async fn recv(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        match self.idevice.read_plist_value().await? {
            Value::Dictionary(d) => Ok(d),
            other => Err(IdeviceError::UnexpectedResponse(format!(
                "restored sent a non-dictionary message: {other:?}"
            ))),
        }
    }
}
