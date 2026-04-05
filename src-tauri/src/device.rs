use std::sync::Mutex;

use idevice::{
    IdeviceService,
    lockdown::LockdownClient,
    provider::UsbmuxdProvider,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tokio_util::sync::CancellationToken;

use crate::pairing::pairing_file;

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub id: u32,
    pub udid: String,
    pub connection_type: String,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfoWithPairing {
    pub info: DeviceInfo,
    pub pairing: Vec<u8>,
}

pub type DeviceInfoMutex = Mutex<Option<DeviceInfoWithPairing>>;
pub type PairingCancelToken = Mutex<Option<CancellationToken>>;

#[tauri::command]
pub async fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    let usbmuxd = UsbmuxdConnection::default().await;
    if usbmuxd.is_err() {
        eprintln!("Failed to connect to usbmuxd: {:?}", usbmuxd.err());
        return Err("Failed to connect to usbmuxd".to_string());
    }
    let mut usbmuxd = usbmuxd.unwrap();

    let devs = usbmuxd.get_devices().await.unwrap();
    if devs.is_empty() {
        return Ok(vec![]);
    }

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
                    eprintln!("Unable to connect to lockdown: {e:?}");
                    return DeviceInfo {
                        connection_type,
                        name: String::from("Unknown Device"),
                        id: device_uid,
                        udid: d.udid.clone(),
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
                udid: d.udid.clone(),
                connection_type,
            }
        })
        .collect();

    Ok(futures::future::join_all(device_info_futures).await)
}

#[tauri::command]
pub async fn set_selected_device(
    app: AppHandle,
    device_state: State<'_, DeviceInfoMutex>,
    cancel_state: State<'_, PairingCancelToken>,
    device: Option<DeviceInfo>,
) -> Result<(), String> {
    if device.is_none() {
        let mut device_state = device_state.lock().unwrap();
        *device_state = None;
        return Ok(());
    }

    let mut usbmuxd = UsbmuxdConnection::default()
        .await
        .map_err(|e| format!("Failed to connect to usbmuxd: {}", e))?;

    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut guard = cancel_state.lock().unwrap();
        if let Some(old) = guard.replace(token.clone()) {
            old.cancel();
        }
    }

    let pairing_result = pairing_file(&app, device.as_ref().unwrap(), &mut usbmuxd, token.clone()).await;

    if !token.is_cancelled() {
        let mut guard = cancel_state.lock().unwrap();
        *guard = None;
    }

    let pairing = pairing_result?;

    let device_with_pairing = DeviceInfoWithPairing {
        info: device.unwrap(),
        pairing,
    };
    let mut device_state = device_state.lock().unwrap();
    *device_state = Some(device_with_pairing);
    Ok(())
}

#[tauri::command]
pub async fn cancel_pairing(cancel_state: State<'_, PairingCancelToken>) -> Result<(), String> {
    let mut guard = cancel_state.lock().unwrap();
    if let Some(token) = guard.take() {
        token.cancel();
    }
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
        .get_device(&device_info.udid)
        .await
        .map_err(|e| format!("Failed to get device: {}", e))?;

    let provider = device.to_provider(UsbmuxdAddr::from_env_var().unwrap(), "iloader");
    Ok(provider)
}
