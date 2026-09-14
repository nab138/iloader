//! Abstraction for preboard

use std::time::Duration;

use plist::Value;
use tracing::{info, warn};

use crate::{Idevice, IdeviceError, IdeviceService, RsdService, obf, restore::RestoreError};

/// Client for interacting with the preboard service on the device.
#[derive(Debug)]
pub struct PreboardServiceClient {
    /// The underlying device connection with established service
    pub idevice: Idevice,
}

impl IdeviceService for PreboardServiceClient {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.preboardservice_v2")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

#[cfg(feature = "rsd")]
impl RsdService for PreboardServiceClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.preboardservice_v2.shim.remote")
    }

    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}

/// The result of a [`create_stashbag`](PreboardServiceClient::create_stashbag) request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashbagOutcome {
    /// The device does not need a stashbag (e.g. no passcode set); nothing further
    /// to do.
    NotRequired,
    /// A stashbag was created and must be committed with the AP ticket once it is
    /// obtained (see [`commit_stashbag`](PreboardServiceClient::commit_stashbag)).
    CommitRequired,
}

impl PreboardServiceClient {
    /// Wraps an already-connected preboard service stream.
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Creates a stashbag, sending the local `manifest` (an unsigned `IM4M` from
    /// `restore::img4::build_preboard_manifest`, available with the `restore`
    /// feature).
    ///
    /// If the device requires a stashbag it shows a passcode prompt on-device; the
    /// user must enter their passcode. Returns once the device reports the outcome.
    ///
    /// # Errors
    /// Returns [`IdeviceError`] on transport failure, if the device reports an
    /// error, or if the user does not enter the passcode in time.
    pub async fn create_stashbag(
        &mut self,
        manifest: &[u8],
    ) -> Result<StashbagOutcome, IdeviceError> {
        self.idevice
            .send_bplist(crate::plist!({
                "Command": "CreateStashbag",
                "Manifest": Value::Data(manifest.to_vec()),
            }))
            .await?;

        // The device streams status messages; wait up to ~130s (the user may be
        // entering a passcode).
        for _ in 0..130 {
            let msg = match tokio::time::timeout(Duration::from_secs(1), self.idevice.read_plist())
                .await
            {
                Ok(msg) => msg?,
                Err(_) => continue, // 1s read timeout: keep waiting
            };

            if bool_field(&msg, "Skip") {
                info!("device does not require a stashbag");
                return Ok(StashbagOutcome::NotRequired);
            }
            if bool_field(&msg, "ShowDialog") {
                info!("device requires a stashbag — enter your passcode on the device");
                continue;
            }
            if let Some(err) = stashbag_error(&msg) {
                return Err(IdeviceError::Restore(RestoreError::Stashbag(format!(
                    "could not create stashbag: {err}"
                ))));
            }
            if bool_field(&msg, "Timeout") {
                return Err(IdeviceError::Restore(RestoreError::Stashbag(
                    "timed out waiting for the passcode to be entered on the device".into(),
                )));
            }
            if bool_field(&msg, "HideDialog") {
                info!("stashbag created");
                return Ok(StashbagOutcome::CommitRequired);
            }
        }

        Err(IdeviceError::Restore(RestoreError::Stashbag(
            "timed out waiting for stashbag creation".into(),
        )))
    }

    /// Commits a previously created stashbag, sending the AP ticket
    /// (`ApImg4Ticket`) as the manifest.
    ///
    /// # Errors
    /// Returns [`IdeviceError`] if the device reports an error or does not confirm
    /// the commit.
    pub async fn commit_stashbag(&mut self, ap_ticket: &[u8]) -> Result<(), IdeviceError> {
        self.idevice
            .send_bplist(crate::plist!({
                "Command": "CommitStashbag",
                "Manifest": Value::Data(ap_ticket.to_vec()),
            }))
            .await?;

        let msg = tokio::time::timeout(Duration::from_secs(30), self.idevice.read_plist())
            .await
            .map_err(|_| {
                IdeviceError::Restore(RestoreError::Stashbag(
                    "timed out committing stashbag".into(),
                ))
            })??;

        if let Some(err) = stashbag_error(&msg) {
            return Err(IdeviceError::Restore(RestoreError::Stashbag(format!(
                "could not commit stashbag: {err}"
            ))));
        }
        if bool_field(&msg, "StashbagCommitComplete") {
            info!("stashbag committed");
            return Ok(());
        }
        warn!("unexpected reply from preboard service: {msg:?}");
        Err(IdeviceError::Restore(RestoreError::Stashbag(
            "preboard service did not confirm the stashbag commit".into(),
        )))
    }
}

/// Reads a boolean field, treating absence as false.
fn bool_field(msg: &plist::Dictionary, key: &str) -> bool {
    msg.get(key).and_then(Value::as_boolean).unwrap_or(false)
}

/// Extracts an error description from a preboard reply, if it reports one.
fn stashbag_error(msg: &plist::Dictionary) -> Option<String> {
    msg.get("Error")?;
    Some(
        msg.get("ErrorString")
            .and_then(Value::as_string)
            .unwrap_or("unknown error")
            .to_string(),
    )
}
