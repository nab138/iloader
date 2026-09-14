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

use crate::{error::AppError, pairing::pairing_file, vision};

/// How a device is reached. Usbmux (iPhone/iPad over USB or Wi-Fi via usbmuxd) is the
/// default; Vision is an Apple Vision Pro reached over an RP tunnel (not usbmux).
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum DeviceTransport {
    #[default]
    Usbmux,
    Vision,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub id: u32,
    pub udid: String,
    pub connection_type: String,
    pub version: String,
    /// Lockdown `DeviceClass` (e.g. "iPhone", "iPad", "RealityDevice"). Optional
    /// because not every device reports it as a string; a missing value must not
    /// drop the device from the list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_class: Option<String>,
    /// Lockdown `ProductType` (e.g. "iPhone14,5", "RealityDevice17,1"). Optional
    /// for the same reason as `device_class`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_type: Option<String>,
    /// Transport used to reach the device. Defaults to usbmux for backwards
    /// compatibility with any persisted iOS device.
    #[serde(default)]
    pub transport: DeviceTransport,
    /// The Vision Pro's IP address (for the RP tunnel). Only set for `Vision`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// For a `Vision` device: whether a reusable pairing file is already stored (so
    /// the frontend can select it directly instead of prompting for the headset
    /// code). Ignored for usbmux devices.
    #[serde(default)]
    pub paired: bool,
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
pub async fn list_devices(app: AppHandle) -> Result<Vec<Result<DeviceInfo, AppError>>, AppError> {
    // usbmux (iPhone/iPad) and Vision Pro (mDNS) discovery are independent transports,
    // so run them concurrently. A usbmux failure must not hide a discovered Vision Pro
    // and vice-versa.
    let (usbmux_devices, vision_devices) = futures::future::join(
        list_usbmux_devices(),
        vision::list_vision_devices(&app),
    )
    .await;

    let mut results: Vec<Result<DeviceInfo, AppError>> = match usbmux_devices {
        Ok(v) => v,
        // No usbmuxd / no iOS devices shouldn't blank out a Vision Pro on Wi-Fi.
        Err(e) => {
            if vision_devices.is_empty() {
                return Err(e);
            }
            Vec::new()
        }
    };
    results.extend(vision_devices.into_iter().map(Ok));
    Ok(results)
}

async fn list_usbmux_devices() -> Result<Vec<Result<DeviceInfo, AppError>>, AppError> {
    let mut usbmuxd = get_usbmuxd().await?;

    let devs = usbmuxd.get_devices().await.map_err(|e| {
        AppError::Usbmuxd("Failed to list devices from usbmuxd".into(), e.to_string())
    })?;
    if devs.is_empty() {
        return Ok(vec![]);
    }

    let usbmuxd_addr = UsbmuxdAddr::from_env_var().map_err(|e| {
        AppError::Usbmuxd(
            "Invalid usbmuxd address from environment".into(),
            e.to_string(),
        )
    })?;

    let device_info_futures: Vec<_> = devs
        .iter()
        .map(|d| {
            let usbmuxd_addr = usbmuxd_addr.clone();
            async move {
                let provider = d.to_provider(usbmuxd_addr, "iloader");
                let device_uid = d.device_id;
                let connection_type = match d.connection_type {
                    Connection::Usb => "USB",
                    Connection::Network(_) => "Network",
                    Connection::Unknown(_) => "Unknown",
                }
                .to_string();

                let mut lockdown_client =
                    LockdownClient::connect(&provider).await.map_err(|e| {
                        eprintln!("Unable to connect to lockdown for {}: {e:?}", d.udid);
                        AppError::DeviceComsWithMessage(
                            "Unable to connect to lockdown".into(),
                            e.to_string(),
                        )
                    })?;

                let device_name_value = lockdown_client
                    .get_value(Some("DeviceName"), None)
                    .await
                    .map_err(|e| {
                    eprintln!("Failed to fetch DeviceName for {}: {e:?}", d.udid);
                    AppError::DeviceComsWithMessage(
                        "Failed to fetch DeviceName".into(),
                        e.to_string(),
                    )
                })?;

                let device_name = device_name_value.as_string().ok_or_else(|| {
                    eprintln!("DeviceName for {} was not a string", d.udid);
                    AppError::DeviceComs("DeviceName was not a string".into())
                })?;

                let version_value = lockdown_client
                    .get_value(Some("ProductVersion"), None)
                    .await
                    .map_err(|e| {
                        eprintln!("Failed to fetch ProductVersion for {}: {e:?}", d.udid);
                        AppError::DeviceComsWithMessage(
                            "Failed to fetch ProductVersion".into(),
                            e.to_string(),
                        )
                    })?;

                let version = version_value.as_string().ok_or_else(|| {
                    eprintln!("ProductVersion for {} was not a string", d.udid);
                    AppError::DeviceComs("Product version was not a string".into())
                })?;

                // DeviceClass / ProductType let us tell an Apple Vision Pro
                // ("RealityDevice" / "RealityDevice17,1") apart from an iPhone or
                // iPad. Read them best-effort: unlike DeviceName/ProductVersion a
                // missing or non-string value must not drop the device.
                let device_class = lockdown_client
                    .get_value(Some("DeviceClass"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(str::to_string));

                let product_type = lockdown_client
                    .get_value(Some("ProductType"), None)
                    .await
                    .ok()
                    .and_then(|v| v.as_string().map(str::to_string));

                Ok::<DeviceInfo, AppError>(DeviceInfo {
                    name: device_name.to_string(),
                    id: device_uid,
                    udid: d.udid.clone(),
                    connection_type,
                    version: version.to_string(),
                    device_class,
                    product_type,
                    transport: DeviceTransport::Usbmux,
                    ip: None,
                    paired: false,
                })
            }
        })
        .collect();

    let device_infos = futures::future::join_all(device_info_futures).await;
    Ok(device_infos)
}

#[tauri::command]
pub async fn set_selected_device(
    app: AppHandle,
    device_state: State<'_, DeviceInfoMutex>,
    cancel_state: State<'_, PairingCancelToken>,
    device: Option<DeviceInfo>,
) -> Result<(), AppError> {
    if device.is_none() {
        let mut device_state = device_state.lock().unwrap();
        *device_state = None;
        return Ok(());
    }

    // Vision Pro: not a usbmux device. Selecting it verifies the stored RP pairing
    // (opening a tunnel to confirm it still pairs) rather than doing lockdown pairing.
    // An unpaired Vision Pro is paired via the `vision_pair` command instead, so a
    // missing pairing here is a real error.
    if device.as_ref().unwrap().transport == DeviceTransport::Vision {
        let dev = device.unwrap();
        let (pairing, udid) = vision::select_vision_device(&app, &dev).await?;
        let mut info = dev;
        info.udid = udid;
        info.paired = true;
        let mut device_state = device_state.lock().unwrap();
        *device_state = Some(DeviceInfoWithPairing { info, pairing });
        return Ok(());
    }

    let mut usbmuxd = get_usbmuxd().await?;

    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut guard = cancel_state.lock().unwrap();
        if let Some(old) = guard.replace(token.clone()) {
            old.cancel();
        }
    }

    let pairing_result =
        pairing_file(&app, device.as_ref().unwrap(), &mut usbmuxd, token.clone()).await;

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
pub async fn cancel_pairing(cancel_state: State<'_, PairingCancelToken>) -> Result<(), AppError> {
    let mut guard = cancel_state.lock().unwrap();
    if let Some(token) = guard.take() {
        token.cancel();
    }
    Ok(())
}

pub async fn get_usbmuxd() -> Result<UsbmuxdConnection, AppError> {
    UsbmuxdConnection::default()
        .await
        .map_err(|e| AppError::Usbmuxd("Failed to connect to usbmuxd".into(), e.to_string()))
}

pub async fn get_provider(device_info: &DeviceInfo) -> Result<UsbmuxdProvider, AppError> {
    get_provider_from_connection(device_info, &mut (get_usbmuxd().await?)).await
}

pub async fn get_provider_from_connection(
    device_info: &DeviceInfo,
    connection: &mut UsbmuxdConnection,
) -> Result<UsbmuxdProvider, AppError> {
    let device = connection
        .get_device(&device_info.udid)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage("Failed to get device".into(), e.to_string())
        })?;

    let provider = device.to_provider(UsbmuxdAddr::from_env_var().unwrap(), "iloader");
    Ok(provider)
}
