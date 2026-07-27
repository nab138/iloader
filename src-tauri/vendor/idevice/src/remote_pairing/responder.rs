//! Device-initiated remote pairing (the "pairable host" / responder side).
//!
//! Starting with iOS 27, a device can initiate pairing to a
//! computer instead of the computer initiating pairing to the device. The
//! computer advertises an `_remotepairing-pairable-host._tcp` mDNS service; the
//! device connects to the advertised port and drives an rppairing conversation.
//!
//! In that conversation the roles are flipped relative to [`super::RemotePairingClient`]:
//! the device plays the rppairing "host" (it sends `originatedBy: "host"`,
//! initiates the handshake and the SRP pair-setup), while the computer
//! play the rppairing "device"/accessory (`originatedBy: "device"`). We generate
//! and display the setup PIN; the user types it into the iOS device.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use idevice_srp::{client::SrpClient, groups::G_3072, server::SrpServer};
use plist::Value;
use plist_macro::{PlistExt, plist};
use rand::{Rng as _, RngExt as _};
use rsa::signature::SignerMut as _;
use sha2::Sha512;
use tracing::{debug, warn};

use crate::IdeviceError;

use super::{
    PeerDevice, RpPairingFile, errors::RemotePairingError, opack, peer_device, tlv,
    tlv::PairingDataComponentType as Tt,
};

/// The DNS-SD service type a pairable host advertises over mDNS. A device browses
/// for this and, when the user taps "Pair with ...", connects to the advertised
/// host/port and drives the rppairing conversation handled by [`PairableHost`].
pub const PAIRABLE_HOST_SERVICE_TYPE: &str = "_remotepairing-pairable-host._tcp.local.";

/// Static information this host advertises and presents to a connecting device.
///
/// `name` and `model` are what the device displays to the user (e.g. a Mac shows
/// `model = "Mac17,7"`, `name = "Jackson's MacBook Pro"`). iOS treats the host as
/// a computer, so keep `model` a Mac identifier. The remaining fields can be left
/// at their defaults as pairing succeeds with empty `udid`/`identifier` and without
/// any `deviceKVSData`.
#[derive(Debug, Clone)]
pub struct PairableHostInfo {
    /// Human-readable name shown on the device, e.g. `"Jackson's MacBook Pro"`.
    pub name: String,
    /// Hardware model identifier shown on the device, e.g. `"Mac17,7"`.
    pub model: String,
    /// UDID reported in `peerDeviceInfo`. May be left empty.
    pub udid: String,
    /// Identifier reported in `peerDeviceInfo`. May be left empty.
    pub identifier: String,
    /// Wire protocol version reported during the handshake. Recent iOS uses 26.
    pub wire_protocol_version: u8,
    /// Our 16-byte mDNS identity key. It is sent to the device during pairing
    /// (M6) and must match the `altIRK` used to compute the `authTag` advertised
    /// in the `_remotepairing-pairable-host._tcp` record, so an already-paired
    /// device can recognize this host. Persist it across runs alongside the
    /// pairing file.
    ///
    /// See [`Self::mdns_txt_records`] and [`crate::remote_pairing::compute_auth_tag`].
    pub alt_irk: [u8; 16],
}

impl PairableHostInfo {
    /// Creates host info for the given name/model with a freshly generated random
    /// `alt_irk`. Persist [`Self::alt_irk`] so reconnecting devices keep working.
    pub fn generate(name: impl Into<String>, model: impl Into<String>) -> Self {
        let mut alt_irk = [0u8; 16];
        rand::rng().fill_bytes(&mut alt_irk);
        Self {
            name: name.into(),
            model: model.into(),
            alt_irk,
            ..Default::default()
        }
    }

    /// Builds the TXT records to publish for the [`PAIRABLE_HOST_SERVICE_TYPE`]
    /// mDNS service, as `(key, value)` pairs suitable for any mDNS library.
    ///
    /// `service_identifier` is the host's stable identifier and must be the same
    /// value used as the mDNS service instance name and sent to the device during
    /// pairing, pass [`RpPairingFile::identifier`]. The `authTag` is derived from
    /// [`Self::alt_irk`] and this identifier so an already-paired device can
    /// recognize the host.
    pub fn mdns_txt_records(&self, service_identifier: &str) -> Vec<(String, String)> {
        let auth_tag = B64.encode(peer_device::compute_auth_tag(
            &self.alt_irk,
            service_identifier,
        ));
        vec![
            ("name".into(), self.name.clone()),
            ("identifier".into(), service_identifier.to_string()),
            ("authTag".into(), auth_tag),
            ("model".into(), self.model.clone()),
            ("flags".into(), "1".into()),
            ("ver".into(), self.wire_protocol_version.to_string()),
            ("minVer".into(), "17".into()),
        ]
    }
}

impl Default for PairableHostInfo {
    fn default() -> Self {
        Self {
            name: "idevice-rs".to_string(),
            model: "Mac17,7".to_string(),
            udid: String::new(),
            identifier: String::new(),
            wire_protocol_version: 26,
            alt_irk: [0u8; 16],
        }
    }
}

/// SRP username used by Apple's pair-setup. Both sides hash with this.
const SRP_USERNAME: &[u8] = b"Pair-Setup";

/// The responder side of rppairing: accepts a device-initiated pairing.
///
/// Construct one around a socket configured for the responder role
/// ([`super::RpPairingSocket::new_device`]) and call [`Self::accept`].
pub struct PairableHost<R: super::RpPairingSocketProvider> {
    inner: R,
    host_info: PairableHostInfo,
    /// Our own send-side sequence counter (the device maintains its own).
    sequence_number: usize,
    /// The SRP session key established during pairing.
    encryption_key: Vec<u8>,
    paired_peer_device: Option<PeerDevice>,
}

impl<R: super::RpPairingSocketProvider> PairableHost<R> {
    pub fn new(inner: R, host_info: PairableHostInfo) -> Self {
        Self {
            inner,
            host_info,
            sequence_number: 0,
            encryption_key: Vec::new(),
            paired_peer_device: None,
        }
    }

    /// The SRP session key established during the most recent successful pairing.
    pub fn encryption_key(&self) -> &[u8] {
        &self.encryption_key
    }

    /// Accept a device-initiated pairing.
    ///
    /// Performs the handshake and the full SRP pair-setup. `pin_callback` is
    /// invoked with the 6-digit setup code that should be displayed to the user
    /// to enter on the device.
    ///
    /// On success the device's identity (including its `altIRK`) is stored in
    /// `pairing_file.alt_irk` and the parsed [`PeerDevice`] is returned. The
    /// caller is responsible for persisting `pairing_file`.
    pub async fn accept<Fut>(
        &mut self,
        pairing_file: &mut RpPairingFile,
        pin_callback: impl FnOnce(String) -> Fut,
    ) -> Result<PeerDevice, IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
    {
        self.handshake().await?;
        let peer_device = self.pair_setup(pairing_file, pin_callback).await?;
        Ok(peer_device)
    }

    /// Receive the device's handshake request and reply with our device info.
    async fn handshake(&mut self) -> Result<(), IdeviceError> {
        debug!("Waiting for device handshake request");
        let request = self.inner.recv_plain().await?;
        debug!("Handshake request: {request:#?}");

        let handshake = request
            .get_by("request")
            .and_then(|x| x.get_by("_0"))
            .and_then(|x| x.get_by("handshake"))
            .and_then(|x| x.get_by("_0"))
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing request._0.handshake._0 in device handshake".into(),
            ))?;

        if handshake
            .get_by("hostOptions")
            .and_then(|x| x.get_by("attemptPairVerify"))
            .and_then(|x| x.as_boolean())
            .unwrap_or(false)
        {
            return Err(IdeviceError::UnexpectedResponse(
                "device requested pair-verify; only device-initiated pair-setup is supported"
                    .into(),
            ));
        }

        let mut peer_info = plist::Dictionary::new();
        peer_info.insert("udid".into(), Value::String(self.host_info.udid.clone()));
        peer_info.insert(
            "deviceKVSIncludesSensitiveInfo".into(),
            Value::Boolean(false),
        );
        peer_info.insert(
            "identifier".into(),
            Value::String(self.host_info.identifier.clone()),
        );
        peer_info.insert("name".into(), Value::String(self.host_info.name.clone()));
        peer_info.insert("model".into(), Value::String(self.host_info.model.clone()));

        let response = plist!({
            "response": {
                "forRequestIdentifier": 0,
                "_1": {
                    "handshake": {
                        "_0": {
                            "wireProtocolVersion": Value::Integer(
                                (self.host_info.wire_protocol_version as i64).into(),
                            ),
                            "minimumSupportedWireProtocolVersion": 8,
                            "deviceOptions": {
                                "allowsPairSetup": true,
                                "allowsPinlessPairing": false,
                                "allowsIncomingTunnelConnections": false,
                                "allowsUpgradeOfLockdownPairings": false,
                                "allowsSharingSensitiveInfo": false
                            },
                            "peerDeviceInfo": Value::Dictionary(peer_info)
                        }
                    }
                }
            }
        });

        debug!("Sending handshake response: {response:#?}");
        self.send_plain(response).await
    }

    /// Run the SRP pair-setup as the server/accessory (M1 - M6).
    async fn pair_setup<Fut>(
        &mut self,
        pairing_file: &mut RpPairingFile,
        pin_callback: impl FnOnce(String) -> Fut,
    ) -> Result<PeerDevice, IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
    {
        debug!("Waiting for pair-setup M1");
        let m1 = self.recv_pairing_tlv().await?;
        expect_state(&m1, 1)?;

        // m2
        let srp_client = SrpClient::<Sha512>::new(&G_3072);
        let srp_server = SrpServer::<Sha512>::new(&G_3072);

        let mut salt = [0u8; 16];
        rand::rng().fill_bytes(&mut salt);

        let pin = format!("{:06}", rand::rng().random_range(0..1_000_000));
        let verifier = srp_client.compute_verifier(SRP_USERNAME, pin.as_bytes(), &salt);

        let (b_priv, b_pub) = loop {
            let mut b = [0u8; 32];
            rand::rng().fill_bytes(&mut b);
            let b_pub = srp_server.compute_public_ephemeral(&b, &verifier);
            if b_pub.len() == 384 {
                break (b, b_pub);
            }
        };

        // Display the PIN for the user to enter on the device.
        pin_callback(pin).await;

        let mut m2 = vec![
            tlv::TLV8Entry {
                tlv_type: Tt::State,
                data: vec![0x02],
            },
            tlv::TLV8Entry {
                tlv_type: Tt::Salt,
                data: salt.to_vec(),
            },
        ];
        m2.extend(chunk_tlv(Tt::PublicKey, &b_pub));
        debug!("Sending pair-setup M2 (salt + B)");
        self.send_pairing_tlv(&m2).await?;

        // m3
        debug!("Waiting for pair-setup M3");
        let m3 = self.recv_pairing_tlv().await?;
        ensure_no_error(&m3)?;
        expect_state(&m3, 3)?;

        let a_pub = tlv::collect_component_data(&m3, Tt::PublicKey);
        let client_proof = tlv::collect_component_data(&m3, Tt::Proof);
        if a_pub.is_empty() || client_proof.is_empty() {
            return Err(IdeviceError::UnexpectedResponse(
                "pair-setup M3 missing public key or proof".into(),
            ));
        }

        let verifier = srp_server
            .process_reply(&b_priv, &verifier, &a_pub, SRP_USERNAME, &salt)
            .map_err(|e| {
                warn!("SRP process_reply failed: {e:?}");
                RemotePairingError::SrpAuthFailed
            })?;

        if verifier.verify_client(&client_proof).is_err() {
            warn!("SRP client proof verification failed (wrong PIN?)");
            // Tell the device authentication failed.
            self.send_pairing_tlv(&[
                tlv::TLV8Entry {
                    tlv_type: Tt::State,
                    data: vec![0x04],
                },
                tlv::TLV8Entry {
                    tlv_type: Tt::ErrorResponse,
                    data: vec![0x02], // kTLVError_Authentication
                },
            ])
            .await?;
            return Err(RemotePairingError::SrpAuthFailed.into());
        }

        let session_key = verifier.key().to_vec();

        // m4
        debug!("Sending pair-setup M4 (server proof)");
        self.send_pairing_tlv(&[
            tlv::TLV8Entry {
                tlv_type: Tt::State,
                data: vec![0x04],
            },
            tlv::TLV8Entry {
                tlv_type: Tt::Proof,
                data: verifier.proof().to_vec(),
            },
        ])
        .await?;

        // Derive the shared ChaCha20-Poly1305 key used for the M5/M6 exchange.
        let setup_cipher = {
            let hk = Hkdf::<Sha512>::new(Some(b"Pair-Setup-Encrypt-Salt"), &session_key);
            let mut key = [0u8; 32];
            hk.expand(b"Pair-Setup-Encrypt-Info", &mut key)
                .expect("HKDF expand failed");
            ChaCha20Poly1305::new(Key::from_slice(&key))
        };

        // m5
        debug!("Waiting for pair-setup M5 (device identity)");
        let m5 = self.recv_pairing_tlv().await?;
        ensure_no_error(&m5)?;
        expect_state(&m5, 5)?;

        let ciphertext = tlv::collect_component_data(&m5, Tt::EncryptedData);
        let plaintext = setup_cipher
            .decrypt(
                Nonce::from_slice(b"\x00\x00\x00\x00PS-Msg05"),
                Payload {
                    msg: &ciphertext,
                    aad: b"",
                },
            )
            .map_err(|e| {
                warn!("Failed to decrypt pair-setup M5: {e:?}");
                RemotePairingError::ChachaEncryption(e)
            })?;

        let device_tlv = tlv::deserialize_tlv8(&plaintext)?;
        debug!("Decrypted device identity TLV: {device_tlv:#?}");
        let peer_device = peer_device::parse_peer_device_from_tlv(&device_tlv)?;

        // m6
        debug!("Sending pair-setup M6 (our identity)");
        let m6_plain = self.build_accessory_identity_tlv(pairing_file, &session_key);
        let m6_cipher = setup_cipher
            .encrypt(
                Nonce::from_slice(b"\x00\x00\x00\x00PS-Msg06"),
                Payload {
                    msg: &m6_plain,
                    aad: b"",
                },
            )
            .map_err(RemotePairingError::ChachaEncryption)?;

        let mut m6 = chunk_tlv(Tt::EncryptedData, &m6_cipher);
        m6.push(tlv::TLV8Entry {
            tlv_type: Tt::State,
            data: vec![0x06],
        });
        self.send_pairing_tlv(&m6).await?;

        // Store the device's altIRK so we can validate its future
        // `_remotepairing._tcp` advertisements.
        pairing_file.alt_irk = Some(peer_device.alt_irk.clone());
        self.encryption_key = session_key;
        self.paired_peer_device = Some(peer_device.clone());

        Ok(peer_device)
    }

    /// Builds the (plaintext) accessory identity TLV sent in M6: our identifier,
    /// long-term Ed25519 public key, a signature over them, and an OPACK info blob.
    fn build_accessory_identity_tlv(
        &self,
        pairing_file: &mut RpPairingFile,
        session_key: &[u8],
    ) -> Vec<u8> {
        // Accessory signature: Ed25519 over (AccessoryX || identifier || LTPK),
        // where AccessoryX is derived from the SRP session key.
        let mut accessory_x = [0u8; 32];
        Hkdf::<Sha512>::new(Some(b"Pair-Setup-Accessory-Sign-Salt"), session_key)
            .expand(b"Pair-Setup-Accessory-Sign-Info", &mut accessory_x)
            .expect("HKDF expand failed");

        let ltpk = pairing_file.e_public_key.to_bytes();
        let mut signbuf = Vec::with_capacity(32 + pairing_file.identifier.len() + 32);
        signbuf.extend_from_slice(&accessory_x);
        signbuf.extend_from_slice(pairing_file.identifier.as_bytes());
        signbuf.extend_from_slice(&ltpk);
        let signature = pairing_file.e_private_key.sign(&signbuf);

        let info = opack::plist_to_opack(&plist!({
            "altIRK": self.host_info.alt_irk.to_vec(),
            "btAddr": "11:22:33:44:55:66",
            "mac": vec![0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66],
            "remotepairing_serial_number": "AAAAAAAAAAAA",
            "accountID": pairing_file.identifier.as_str(),
            "remotepairing_udid": self.host_info.udid.as_str(),
            "model": self.host_info.model.as_str(),
            "name": self.host_info.name.as_str()
        }));

        tlv::serialize_tlv8(&[
            tlv::TLV8Entry {
                tlv_type: Tt::Identifier,
                data: pairing_file.identifier.as_bytes().to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: Tt::PublicKey,
                data: ltpk.to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: Tt::Signature,
                data: signature.to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: Tt::Info,
                data: info,
            },
        ])
    }

    /// Send a plain (`message.plain`) envelope, bumping our sequence counter.
    async fn send_plain(&mut self, value: plist::Value) -> Result<(), IdeviceError> {
        self.inner.send_plain(value, self.sequence_number).await?;
        self.sequence_number += 1;
        Ok(())
    }

    /// Send a `pairingData` event carrying the given TLV entries.
    async fn send_pairing_tlv(&mut self, entries: &[tlv::TLV8Entry]) -> Result<(), IdeviceError> {
        let data = R::serialize_bytes(&tlv::serialize_tlv8(entries));
        self.send_plain(plist!({
            "event": {
                "_0": {
                    "pairingData": {
                        "_0": {
                            "data": data,
                            "startNewSession": false,
                            "kind": "setupManualPairing"
                        }
                    }
                }
            }
        }))
        .await
    }

    /// Receive a `pairingData` event and decode its TLV payload.
    async fn recv_pairing_tlv(&mut self) -> Result<Vec<tlv::TLV8Entry>, IdeviceError> {
        let response = self.inner.recv_plain().await?;

        let data = response
            .get_by("event")
            .and_then(|x| x.get_by("_0"))
            .and_then(|x| x.get_by("pairingData"))
            .and_then(|x| x.get_by("_0"))
            .and_then(|x| x.get_by("data"))
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing event._0.pairingData._0.data in device message".into(),
            ))?;

        let bytes = R::deserialize_bytes(data.to_owned()).ok_or(
            IdeviceError::UnexpectedResponse("failed to deserialize pairing data bytes".into()),
        )?;

        Ok(tlv::deserialize_tlv8(&bytes)?)
    }
}

/// Split `data` into TLV entries of at most 255 bytes each (the TLV length limit).
fn chunk_tlv(tlv_type: Tt, data: &[u8]) -> Vec<tlv::TLV8Entry> {
    data.chunks(255)
        .map(|chunk| tlv::TLV8Entry {
            tlv_type,
            data: chunk.to_vec(),
        })
        .collect()
}

/// Returns an error if the TLV stream contains an `ErrorResponse` component.
fn ensure_no_error(entries: &[tlv::TLV8Entry]) -> Result<(), IdeviceError> {
    if let Some(err) = entries.iter().find(|e| e.tlv_type == Tt::ErrorResponse) {
        return Err(IdeviceError::UnexpectedResponse(format!(
            "device returned pairing error: {:?}",
            err.data
        )));
    }
    Ok(())
}

/// Validates that the TLV stream carries the expected `State` value.
fn expect_state(entries: &[tlv::TLV8Entry], expected: u8) -> Result<(), IdeviceError> {
    let state = entries
        .iter()
        .find(|e| e.tlv_type == Tt::State)
        .and_then(|e| e.data.first().copied());
    match state {
        Some(s) if s == expected => Ok(()),
        other => Err(IdeviceError::UnexpectedResponse(format!(
            "unexpected pair-setup state: expected {expected}, got {other:?}"
        ))),
    }
}

impl<R: super::RpPairingSocketProvider> std::fmt::Debug for PairableHost<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairableHost")
            .field("inner", &self.inner)
            .field("sequence_number", &self.sequence_number)
            .field("host_info", &self.host_info)
            .finish()
    }
}
