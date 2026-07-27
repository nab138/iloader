//! iOS Lockdown Service Client
//!
//! Provides functionality for interacting with the lockdown service on iOS devices,
//! which is the primary service for device management and service discovery.

use plist::Value;
use tracing::error;

use crate::{Idevice, IdeviceError, IdeviceService, obf, pairing_file};

/// Client for interacting with the iOS lockdown service
///
/// This is the primary service for device management and provides:
/// - Access to device information and settings
/// - Service discovery and port allocation
/// - Session management and security
#[derive(Debug)]
pub struct LockdownClient {
    /// The underlying device connection with established lockdown service
    pub idevice: crate::Idevice,
}

#[cfg(feature = "rsd")]
impl crate::RsdService for LockdownClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.lockdown.remote.trusted")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}

impl IdeviceService for LockdownClient {
    /// Returns the lockdown service name as registered with the device
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.lockdown")
    }

    /// Establishes a connection to the lockdown service
    ///
    /// # Arguments
    /// * `provider` - Device connection provider
    ///
    /// # Returns
    /// A connected `LockdownClient` instance
    ///
    /// # Errors
    /// Returns `IdeviceError` if connection fails
    async fn connect(
        provider: &dyn crate::provider::IdeviceProvider,
    ) -> Result<Self, IdeviceError> {
        let idevice = provider.connect(Self::LOCKDOWND_PORT).await?;
        Ok(Self::new(idevice))
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl LockdownClient {
    /// The default TCP port for the lockdown service
    pub const LOCKDOWND_PORT: u16 = 62078;

    /// Creates a new lockdown client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Retrieves a specific value from the device
    ///
    /// # Arguments
    /// * `value` - The name of the value to retrieve (e.g., "DeviceName")
    ///
    /// # Returns
    /// The requested value as a plist Value
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The requested value doesn't exist
    /// - The response is malformed
    ///
    /// # Example
    /// ```rust
    /// let device_name = client.get_value("DeviceName").await?;
    /// println!("Device name: {:?}", device_name);
    /// ```
    pub async fn get_value(
        &mut self,
        key: Option<&str>,
        domain: Option<&str>,
    ) -> Result<Value, IdeviceError> {
        let request = crate::plist!({
            "Label": self.idevice.label.clone(),
            "Request": "GetValue",
            "Key":? key,
            "Domain":? domain
        });
        self.idevice.send_plist(request).await?;
        let message: plist::Dictionary = self.idevice.read_plist().await?;
        match message.get("Value") {
            Some(m) => Ok(m.to_owned()),
            None => Err(IdeviceError::UnexpectedResponse(
                "missing Value in GetValue response".into(),
            )),
        }
    }

    /// Sets a value on the device
    ///
    /// # Arguments
    /// * `key` - The key to set
    /// * `value` - The plist value to set
    /// * `domain` - An optional domain to set by
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The response is malformed
    ///
    /// # Example
    /// ```rust
    /// client.set_value("EnableWifiDebugging", true.into(), Some("com.apple.mobile.wireless_lockdown".to_string())).await?;
    /// ```
    pub async fn set_value(
        &mut self,
        key: impl Into<String>,
        value: Value,
        domain: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let key = key.into();

        let req = crate::plist!({
            "Label": self.idevice.label.clone(),
            "Request": "SetValue",
            "Key": key,
            "Value": value,
            "Domain":? domain
        });

        self.idevice.send_plist(req).await?;
        self.idevice.read_plist().await?;

        Ok(())
    }

    /// Removes a value on the device
    ///
    /// Sends a lockdown `RemoveValue` request. Wire format matches Apple's
    /// `AMDeviceRemoveValue`: `{ "Request": "RemoveValue", "Domain"?: domain, "Key": key }`.
    ///
    /// # Arguments
    /// * `key` - The key to remove
    /// * `domain` - An optional domain to remove from
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or the device replies with an `Error`
    pub async fn remove_value(
        &mut self,
        key: impl Into<String>,
        domain: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let key = key.into();

        let req = crate::plist!({
            "Label": self.idevice.label.clone(),
            "Request": "RemoveValue",
            "Key": key,
            "Domain":? domain
        });

        self.idevice.send_plist(req).await?;
        let response: plist::Dictionary = self.idevice.read_plist().await?;
        if let Some(plist::Value::String(e)) = response.get("Error") {
            return Err(IdeviceError::UnexpectedResponse(format!(
                "RemoveValue failed: {e}"
            )));
        }

        Ok(())
    }

    /// Unpairs the host from the device
    ///
    /// Sends a lockdown `Unpair` request carrying the host pair-record identity.
    ///
    /// Note this only removes the pairing from the device side. The host-side pairing record
    /// (stored by usbmuxd) should be removed separately via
    /// [`crate::usbmuxd::UsbmuxdConnection::delete_pair_record`].
    ///
    /// # Arguments
    /// * `host_id` - The `HostID` from the pairing record (see
    ///   [`crate::pairing_file::PairingFile::host_id`])
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or the device replies with an `Error`.
    pub async fn unpair(&mut self, host_id: impl Into<String>) -> Result<(), IdeviceError> {
        let host_id = host_id.into();

        let req = crate::plist!({
            "Label": self.idevice.label.clone(),
            "Request": "Unpair",
            "PairRecord": {
                "HostID": host_id
            }
        });

        self.idevice.send_plist(req).await?;
        let response: plist::Dictionary = self.idevice.read_plist().await?;
        if let Some(plist::Value::String(e)) = response.get("Error") {
            return Err(IdeviceError::UnexpectedResponse(format!(
                "Unpair failed: {e}"
            )));
        }

        Ok(())
    }

    /// Starts a secure TLS session with the device
    ///
    /// # Arguments
    /// * `pairing_file` - Contains the device's identity and certificates
    ///
    /// # Returns
    /// `Ok(legacy)` on successful session establishment, where `legacy` indicates
    /// whether the device is running iOS < 5 and requires legacy TLS settings
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - No connection is established
    /// - The session request is denied
    /// - TLS handshake fails
    pub async fn start_session(
        &mut self,
        pairing_file: &pairing_file::PairingFile,
    ) -> Result<bool, IdeviceError> {
        if self.idevice.socket.is_none() {
            return Err(IdeviceError::NoEstablishedConnection);
        }

        let legacy = self
            .get_value(Some("ProductVersion"), None)
            .await
            .ok()
            .as_ref()
            .and_then(|x| x.as_string())
            .and_then(|x| x.split(".").next())
            .and_then(|x| x.parse::<u8>().ok())
            .map(|x| x < 5)
            .unwrap_or(false);

        let request = crate::plist!({
            "Label": self.idevice.label.clone(),
            "Request": "StartSession",
            "HostID": pairing_file.host_id.clone(),
            "SystemBUID": pairing_file.system_buid.clone()

        });
        self.idevice.send_plist(request).await?;

        let response = self.idevice.read_plist().await?;
        match response.get("EnableSessionSSL") {
            Some(plist::Value::Boolean(enable)) => {
                if !enable {
                    return Err(IdeviceError::UnexpectedResponse(
                        "EnableSessionSSL is false in StartSession response".into(),
                    ));
                }
            }
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing EnableSessionSSL in StartSession response".into(),
                ));
            }
        }

        self.idevice.start_session(pairing_file, legacy).await?;
        Ok(legacy)
    }

    /// Requests to start a service on the device
    ///
    /// # Arguments
    /// * `identifier` - The service identifier (e.g., "com.apple.debugserver")
    ///
    /// # Returns
    /// A tuple containing:
    /// - The port number where the service is available
    /// - A boolean indicating whether SSL should be used
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - The service cannot be started
    /// - The response is malformed
    /// - The requested service doesn't exist
    pub async fn start_service(
        &mut self,
        identifier: impl Into<String>,
    ) -> Result<(u16, bool), IdeviceError> {
        self.start_service_with_escrow(identifier, None).await
    }

    /// Requests to start a service, optionally presenting an escrow keybag so the device unlocks
    /// its data protection for the service session.
    ///
    /// This mirrors Apple's `AMDeviceSecureStartService` with `UnlockEscrowBag = true`: the
    /// `StartService` lockdown request gains an `EscrowBag` key. Without it the device leaves data
    /// protection locked, so a service that must write protection-class files (mobilebackup2
    /// restore of an *encrypted* backup, house_arrest) fails device-side with
    /// "setting protection class (device locked?)" (MBErrorDomain 208). Non-protected data is
    /// unaffected, which is why a non-encrypted restore succeeds without the escrow bag.
    ///
    /// # Arguments
    /// * `identifier` - The service identifier (e.g., "com.apple.mobilebackup2")
    /// * `escrow_bag` - The device's escrow keybag from the pairing record; `None` behaves exactly
    ///   like [`start_service`](Self::start_service).
    pub async fn start_service_with_escrow(
        &mut self,
        identifier: impl Into<String>,
        escrow_bag: Option<Vec<u8>>,
    ) -> Result<(u16, bool), IdeviceError> {
        let identifier = identifier.into();
        let mut req = crate::plist!({
            "Request": "StartService",
            "Service": identifier,
        });
        if let (Some(bag), plist::Value::Dictionary(d)) = (escrow_bag, &mut req) {
            d.insert("EscrowBag".into(), plist::Value::Data(bag));
        }
        self.idevice.send_plist(req).await?;
        let response = self.idevice.read_plist().await?;

        let ssl = match response.get("EnableServiceSSL") {
            Some(plist::Value::Boolean(ssl)) => ssl.to_owned(),
            _ => false, // over USB, this option won't exist
        };

        match response.get("Port") {
            Some(plist::Value::Integer(port)) => {
                if let Some(port) = port.as_unsigned() {
                    Ok((port as u16, ssl))
                } else {
                    error!("Port isn't an unsigned integer!");
                    Err(IdeviceError::UnexpectedResponse(
                        "Port is not an unsigned integer in StartService response".into(),
                    ))
                }
            }
            _ => {
                error!("Response didn't contain an integer port");
                Err(IdeviceError::UnexpectedResponse(
                    "missing Port in StartService response".into(),
                ))
            }
        }
    }

    /// Generates a pairing file and sends it to the device for trusting.
    /// Note that this does NOT save the file to usbmuxd's cache. That's a responsibility of the
    /// caller.
    /// Note that this function is computationally heavy in a debug build.
    ///
    /// # Arguments
    /// * `host_id` - The host ID, in the form of a UUID. Typically generated from the host name
    /// * `system_buid` - UUID fetched from usbmuxd. Doesn't appear to affect function.
    ///
    /// # Returns
    /// The newly generated pairing record
    ///
    /// # Errors
    /// Returns `IdeviceError`
    #[cfg(feature = "pair")]
    pub async fn pair(
        &mut self,
        host_id: impl Into<String>,
        system_buid: impl Into<String>,
        host_name: Option<&str>,
    ) -> Result<crate::pairing_file::PairingFile, IdeviceError> {
        let host_id = host_id.into();
        let system_buid = system_buid.into();

        let (req, mut pair_record, private_key) = self
            .build_pair_request(&host_id, &system_buid, host_name)
            .await?;

        loop {
            self.idevice.send_plist(req.clone()).await?;
            match self.idevice.read_plist().await {
                Ok(escrow) => {
                    pair_record.insert(
                        "HostPrivateKey".into(),
                        plist::Value::Data(private_key.clone()),
                    );
                    if let Some(escrow) = escrow.get("EscrowBag").and_then(|x| x.as_data()) {
                        pair_record.insert("EscrowBag".into(), plist::Value::Data(escrow.to_vec()));
                    }

                    let p = crate::pairing_file::PairingFile::from_value(
                        &plist::Value::Dictionary(pair_record),
                    )?;

                    break Ok(p);
                }
                Err(IdeviceError::PairingDialogResponsePending) => {
                    crate::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Err(e) => break Err(e),
            }
        }
    }

    /// Builds the lockdown `Pair` request and the accompanying host pair record.
    ///
    /// This fetches the device public key and WiFi MAC, generates the pairing certificates, and
    /// assembles the pair record (WITHOUT `HostPrivateKey`, which is inserted only after the device
    /// accepts the pairing) plus the outgoing request.
    ///
    /// # Returns
    /// A tuple of `(request, pair_record_without_host_private_key, ca_private_key)`.
    #[cfg(feature = "pair")]
    async fn build_pair_request(
        &mut self,
        host_id: &str,
        system_buid: &str,
        host_name: Option<&str>,
    ) -> Result<(plist::Value, plist::Dictionary, Vec<u8>), IdeviceError> {
        let pub_key = self.get_value(Some("DevicePublicKey"), None).await?;
        let pub_key = match pub_key.as_data().map(|x| x.to_vec()) {
            Some(p) => p,
            None => {
                tracing::warn!("Did not get public key data response");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing DevicePublicKey data in pair response".into(),
                ));
            }
        };

        let wifi_mac = self.get_value(Some("WiFiAddress"), None).await?;
        let wifi_mac = match wifi_mac.as_string() {
            Some(w) => w,
            None => {
                tracing::warn!("Did not get WiFiAddress string");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing WiFiAddress string in pair response".into(),
                ));
            }
        };

        let ca = crate::ca::generate_certificates(&pub_key, None).unwrap();
        let pair_record = crate::plist!(dict {
            "DevicePublicKey": pub_key,
            "DeviceCertificate": ca.dev_cert,
            "HostCertificate": ca.host_cert.clone(),
            "HostID": host_id,
            "RootCertificate": ca.host_cert,
            "RootPrivateKey": ca.private_key.clone(),
            "WiFiMACAddress": wifi_mac,
            "SystemBUID": system_buid,
        });

        let req = crate::plist!({
            "Label": self.idevice.label.clone(),
            "Request": "Pair",
            "HostName":? host_name,
            "PairRecord": pair_record.clone(),
            "ProtocolVersion": "2",
            "PairingOptions": {
                "ExtendedPairingErrors": true
            }
        });

        Ok((req, pair_record, ca.private_key))
    }

    /// Attempts to pair with the device exactly ONCE, without looping or sleeping.
    ///
    /// Unlike [`LockdownClient::pair`], this does NOT block on
    /// [`IdeviceError::PairingDialogResponsePending`]: the pending/denied/password-protected
    /// errors are propagated directly to the caller so it can drive its own retry/UI flow (e.g.
    /// showing a "Trust This Computer" prompt). The lockdown "Error" field is mapped to the
    /// corresponding [`IdeviceError`] variant by `read_plist`.
    ///
    /// Note that this does NOT save the file to usbmuxd's cache. That's a responsibility of the
    /// caller.
    ///
    /// # Arguments
    /// * `host_id` - The host ID, in the form of a UUID. Typically generated from the host name
    /// * `system_buid` - UUID fetched from usbmuxd. Doesn't appear to affect function.
    ///
    /// # Returns
    /// The newly generated pairing record on success.
    ///
    /// # Errors
    /// Returns `IdeviceError`, including `PairingDialogResponsePending`, `UserDeniedPairing`, and
    /// `PasswordProtected` surfaced directly from the device.
    #[cfg(feature = "pair")]
    pub async fn pair_once(
        &mut self,
        host_id: impl Into<String>,
        system_buid: impl Into<String>,
        host_name: Option<&str>,
    ) -> Result<crate::pairing_file::PairingFile, IdeviceError> {
        let host_id = host_id.into();
        let system_buid = system_buid.into();

        let (req, mut pair_record, private_key) = self
            .build_pair_request(&host_id, &system_buid, host_name)
            .await?;

        self.idevice.send_plist(req).await?;
        // Propagates Err(PairingDialogResponsePending)/UserDeniedPairing/PasswordProtected etc.
        let escrow = self.idevice.read_plist().await?;

        pair_record.insert("HostPrivateKey".into(), plist::Value::Data(private_key));
        if let Some(escrow) = escrow.get("EscrowBag").and_then(|x| x.as_data()) {
            pair_record.insert("EscrowBag".into(), plist::Value::Data(escrow.to_vec()));
        }

        crate::pairing_file::PairingFile::from_value(&plist::Value::Dictionary(pair_record))
    }

    /// Tell the device to enter recovery mode
    pub async fn enter_recovery(&mut self) -> Result<(), IdeviceError> {
        self.idevice
            .send_plist(crate::plist!({
                "Request": "EnterRecovery"
            }))
            .await?;

        let res = self.idevice.read_plist().await?;

        if res.get("Request").and_then(|x| x.as_string()) == Some("EnterRecovery") {
            Ok(())
        } else {
            Err(IdeviceError::UnexpectedResponse(
                "EnterRecovery request not acknowledged".into(),
            ))
        }
    }
}

impl From<Idevice> for LockdownClient {
    /// Converts an existing device connection into a lockdown client
    fn from(value: Idevice) -> Self {
        Self::new(value)
    }
}
