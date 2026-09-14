use crate::{
    SideloadError,
    dev::{
        developer_session::DeveloperSession,
        device_type::{DeveloperDeviceType, dev_url},
        teams::DeveloperTeam,
    },
};
use plist_macro::plist;
use rootcause::prelude::*;
use serde::Deserialize;
use tracing::info;

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeveloperDevice {
    pub name: Option<String>,
    pub device_id: Option<String>,
    pub device_number: String,
    pub status: Option<String>,
}

#[cfg_attr(feature = "wasm", async_trait::async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait::async_trait)]
pub trait DevicesApi {
    fn developer_session(&mut self) -> &mut DeveloperSession;

    async fn list_devices(
        &mut self,
        team: &DeveloperTeam,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<Vec<DeveloperDevice>, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
        });

        let devices: Vec<DeveloperDevice> = self
            .developer_session()
            .send_dev_request(&dev_url("listDevices", device_type), body, "devices")
            .await
            .context("Failed to list developer devices")?;

        Ok(devices)
    }

    async fn add_device(
        &mut self,
        team: &DeveloperTeam,
        name: &str,
        udid: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<DeveloperDevice, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "name": name,
            "deviceNumber": udid,
        });

        let device: DeveloperDevice = self
            .developer_session()
            .send_dev_request(&dev_url("addDevice", device_type), body, "device")
            .await
            .context("Failed to add developer device")?;

        Ok(device)
    }

    // TODO: This can be skipped if we know the device is already registered
    /// Check if the device is a development device, and add it if not
    async fn ensure_device_registered(
        &mut self,
        team: &DeveloperTeam,
        name: &str,
        udid: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<(), Report> {
        let device_type = device_type.into();
        let devices = self.list_devices(team, device_type.clone()).await?;

        if devices.iter().any(|d| d.device_number == udid) {
            info!("Device is a development device");
            return Ok(());
        }

        info!("Registering development device");
        if let Err(e) = self.add_device(team, name, udid, device_type).await {
            let already_registered = e
                .iter_reports()
                .find_map(|node| node.downcast_current_context::<SideloadError>())
                .is_some_and(|err| matches!(err, SideloadError::DeveloperError(35, _)));
            if !already_registered {
                return Err(e);
            }
            info!("Device already registered on team");
        }

        Ok(())
    }
}

impl DevicesApi for DeveloperSession {
    fn developer_session(&mut self) -> &mut DeveloperSession {
        self
    }
}
