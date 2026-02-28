use std::sync::Mutex;

use idevice::{
    IdeviceService,
    lockdown::LockdownClient,
    provider::UsbmuxdProvider,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::{debug, error, warn};

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub id: u32,
    pub uuid: String,
    pub connection_type: String,
}

pub type DeviceInfoMutex = Mutex<Option<DeviceInfo>>;

#[tauri::command]
pub async fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    let mut usbmuxd = UsbmuxdConnection::default().await.map_err(|e| {
        error!(target: "device", error = %e, "Failed to connect to usbmuxd");
        format!("Failed to connect to usbmuxd: {}", e)
    })?;

    let devs = usbmuxd.get_devices().await.map_err(|e| {
        error!(target: "device", error = %e, "Failed to list devices");
        format!("Failed to list devices: {}", e)
    })?;
    if devs.is_empty() {
        debug!(target: "device", "No devices found");
        return Ok(vec![]);
    }
    debug!(target: "device", count = devs.len(), "Found devices");

    let device_info_futures: Vec<_> = devs
        .iter()
        .map(|d| async move {
            let provider = d.to_provider(UsbmuxdAddr::from_env_var().unwrap(), "iloader");
            let device_uid = d.device_id;
            let connection_type = match d.connection_type {
                Connection::Usb => "USB",
                Connection::Network(_) => "Network",
                Connection::Unknown(_) => "Unknown",
            }
            .to_string();

            let mut lockdown_client = match LockdownClient::connect(&provider).await {
                Ok(l) => l,
                Err(e) => {
                    warn!(
                        target: "device",
                        udid = %d.udid,
                        device_id = device_uid,
                        error = %e,
                        "Unable to connect to lockdown — returning Unknown Device"
                    );
                    return DeviceInfo {
                        connection_type,
                        name: String::from("Unknown Device"),
                        id: device_uid,
                        uuid: d.udid.clone(),
                    };
                }
            };

            let device_name = lockdown_client
                .get_value(Some("DeviceName"), None)
                .await
                .expect("Failed to get device name")
                .as_string()
                .expect("Failed to convert device name to string")
                .to_string();

            DeviceInfo {
                name: device_name,
                id: device_uid,
                uuid: d.udid.clone(),
                connection_type,
            }
        })
        .collect();

    Ok(futures::future::join_all(device_info_futures).await)
}

#[tauri::command]
pub async fn set_selected_device(
    device_state: State<'_, DeviceInfoMutex>,
    device: Option<DeviceInfo>,
) -> Result<(), String> {
    let mut device_state = device_state.lock().unwrap();
    *device_state = device;
    Ok(())
}

pub async fn get_provider(device_info: &DeviceInfo) -> Result<UsbmuxdProvider, String> {
    let mut usbmuxd = UsbmuxdConnection::default()
        .await
        .map_err(|e| format!("Failed to connect to usbmuxd: {}", e))?;

    get_provider_from_connection(device_info, &mut usbmuxd).await
}

pub async fn get_provider_from_connection(
    device_info: &DeviceInfo,
    connection: &mut UsbmuxdConnection,
) -> Result<UsbmuxdProvider, String> {
    let device = connection
        .get_device(&device_info.uuid)
        .await
        .map_err(|e| format!("Failed to get device: {}", e))?;

    let provider = device.to_provider(UsbmuxdAddr::from_env_var().unwrap(), "iloader");
    Ok(provider)
}
