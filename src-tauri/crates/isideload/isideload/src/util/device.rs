use idevice::{IdeviceService, lockdown::LockdownClient, provider::IdeviceProvider};
use rootcause::prelude::*;

pub struct IdeviceInfo {
    pub name: String,
    pub udid: String,
}

impl IdeviceInfo {
    pub fn new(name: String, udid: String) -> Self {
        Self { name, udid }
    }

    pub async fn from_device(device: &impl IdeviceProvider) -> Result<Self, Report> {
        let mut lockdown = LockdownClient::connect(device)
            .await
            .context("Failed to connect to device lockdown")?;
        let pairing = device
            .get_pairing_file()
            .await
            .context("Failed to get device pairing file")?;
        lockdown
            .start_session(&pairing)
            .await
            .context("Failed to start lockdown session")?;
        let device_name = lockdown
            .get_value(Some("DeviceName"), None)
            .await
            .context("Failed to get device name")?
            .as_string()
            .ok_or_else(|| report!("Device name is not a string"))?
            .to_string();

        let device_udid = lockdown
            .get_value(Some("UniqueDeviceID"), None)
            .await
            .context("Failed to get device UDID")?
            .as_string()
            .ok_or_else(|| report!("Device UDID is not a string"))?
            .to_string();

        Ok(Self::new(device_name, device_udid))
    }
}
