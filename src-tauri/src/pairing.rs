use std::collections::HashMap;

// used https://github.com/jkcoxson/idevice_pair/ as a guide
use idevice::{
    IdeviceError, IdeviceService, house_arrest::HouseArrestClient,
    installation_proxy::InstallationProxyClient, lockdown::LockdownClient,
    pairing_file::PairingFile, usbmuxd::UsbmuxdConnection,
};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tracing::{debug, error, info, warn};

use crate::device::{DeviceInfo, DeviceInfoMutex, get_provider, get_provider_from_connection};

const PAIRING_APPS: &[(&str, &str)] = &[
    ("SideStore", "ALTPairingFile.mobiledevicepairing"),
    (
        "LiveContainer",
        "SideStore/Documents/ALTPairingFile.mobiledevicepairing",
    ),
    ("Feather", "pairingFile.plist"),
    ("StikDebug", "pairingFile.plist"),
    ("StikDebug (Sideloaded)", "pairingFile.plist"),
    ("StikTest", "stiktest_pairing.plist"),
    ("Protokolle", "pairingFile.plist"),
    ("Antrag", "pairingFile.plist"),
    ("SparseBox", "pairingFile.plist"),
    ("StikStore", "pairingFile.plist"),
    ("ByeTunes", "pairing file/pairingFile.plist"),
];

/// Re-pair the device: generates fresh certificates, prompts Trust on the device,
/// and saves the new pair record to usbmuxd. Returns the new PairingFile.
async fn repair_device(
    device: &DeviceInfo,
    usbmuxd: &mut UsbmuxdConnection,
) -> Result<PairingFile, String> {
    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        "Starting device re-pair (InvalidHostID recovery)"
    );

    let provider = get_provider(device).await?;

    let host_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let system_buid = usbmuxd.get_buid().await.map_err(|e| {
        error!(target: "pairing", error = %e, "Failed to get SystemBUID from usbmuxd");
        format!("Failed to get SystemBUID: {}", e)
    })?;

    debug!(
        target: "pairing",
        new_host_id = %host_id,
        system_buid = %system_buid,
        "Generated new HostID for re-pair"
    );

    let mut lc = LockdownClient::connect(&provider).await.map_err(|e| {
        error!(target: "pairing", error = %e, "Failed to connect to lockdown for re-pair");
        format!("Failed to connect to lockdown for re-pair: {}", e)
    })?;

    info!(
        target: "pairing",
        device_name = %device.name,
        "Sending pair request — unlock the device and tap Trust if prompted"
    );

    let mut pairing_file = lc.pair(&host_id, &system_buid, Some("iLoader")).await.map_err(|e| {
        match e {
            IdeviceError::UserDeniedPairing => {
                warn!(target: "pairing", device_name = %device.name, "User denied the Trust dialog");
                "Pairing denied: you tapped \"Don't Trust\" on the device. Replug and try again, then tap Trust.".to_string()
            }
            IdeviceError::PasswordProtected => {
                warn!(target: "pairing", device_name = %device.name, "Device requires passcode to pair");
                "Device is passcode-protected. Unlock the device, then retry.".to_string()
            }
            IdeviceError::DeviceLocked => {
                warn!(target: "pairing", device_name = %device.name, "Device is locked during pair");
                "Device is locked. Unlock the screen and retry.".to_string()
            }
            _ => {
                error!(target: "pairing", device_name = %device.name, error = %e, "Re-pair failed");
                format!("Re-pair failed: {}", e)
            }
        }
    })?;

    pairing_file.udid = Some(provider.udid.clone());

    // Save the new pair record to usbmuxd so future connections use it
    let serialized = pairing_file.clone().serialize().map_err(|e| {
        error!(target: "pairing", error = %e, "Failed to serialize new pair record");
        format!("Failed to serialize new pair record: {}", e)
    })?;

    usbmuxd.save_pair_record(&provider.udid, serialized).await.map_err(|e| {
        error!(target: "pairing", error = %e, "Failed to save new pair record to usbmuxd");
        format!("Failed to save new pair record to usbmuxd: {}", e)
    })?;

    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        new_host_id = %host_id,
        "Device re-paired successfully — new pair record saved"
    );

    Ok(pairing_file)
}

async fn pairing_file(
    device: DeviceInfo,
    usbmuxd: &mut UsbmuxdConnection,
) -> Result<PairingFile, String> {
    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        "Starting pairing file retrieval"
    );

    let provider = get_provider(&device).await?;
    debug!(
        target: "pairing",
        provider_udid = %provider.udid,
        "Resolved provider for pairing file retrieval"
    );

    let mut pairing_file = match usbmuxd.get_pair_record(&provider.udid).await {
        Ok(mut pf) => {
            pf.udid = Some(provider.udid.clone());
            debug!(
                target: "pairing",
                has_host_id = !pf.host_id.is_empty(),
                has_system_buid = !pf.system_buid.is_empty(),
                "Loaded pair record and injected UDID"
            );
            pf
        }
        Err(e) => {
            warn!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                error = %e,
                "No pair record found — initiating fresh pairing"
            );
            // No pair record at all (fresh computer). Pair from scratch.
            repair_device(&device, usbmuxd).await?
        }
    };

    let mut lc = LockdownClient::connect(&provider)
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                error = %e,
                "Failed to connect to lockdown service"
            );
            format!(
                "Failed to connect to lockdown for device {} (uuid: {}, udid: {}): {}",
                device.name, device.uuid, provider.udid, e
            )
        })?;

    debug!(
        target: "pairing",
        provider_udid = %provider.udid,
        "Connected to lockdown, starting session"
    );

    match lc.start_session(&pairing_file).await {
        Ok(()) => {
            info!(
                target: "pairing",
                provider_udid = %provider.udid,
                "Lockdown session started"
            );
        }
        Err(IdeviceError::InvalidHostID) => {
            warn!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                "InvalidHostID — attempting automatic re-pair"
            );

            // Re-pair the device to get a fresh trust relationship
            pairing_file = repair_device(&device, usbmuxd).await?;

            // Reconnect lockdown with the new pairing file
            lc = LockdownClient::connect(&provider).await.map_err(|e| {
                error!(target: "pairing", error = %e, "Failed to reconnect lockdown after re-pair");
                format!("Failed to reconnect lockdown after re-pair: {}", e)
            })?;

            lc.start_session(&pairing_file).await.map_err(|e| {
                error!(target: "pairing", error = %e, "Session still fails after re-pair");
                format!(
                    "Failed to start session even after re-pairing device {} (uuid: {}): {}",
                    device.name, device.uuid, e
                )
            })?;

            info!(
                target: "pairing",
                provider_udid = %provider.udid,
                "Lockdown session started after successful re-pair"
            );
        }
        Err(IdeviceError::DeviceLocked) => {
            return Err(format!(
                "Failed to start lockdown session for device {} (uuid: {}, udid: {}): device locked\n\n\
                DeviceLocked: unlock the device screen and try again.",
                device.name, device.uuid, provider.udid
            ));
        }
        Err(e) => {
            warn!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                error = %e,
                "Session failed — attempting re-pair as fallback"
            );

            // Try re-pair for any session error (stale pair record, etc.)
            match repair_device(&device, usbmuxd).await {
                Ok(new_pf) => {
                    pairing_file = new_pf;
                    lc = LockdownClient::connect(&provider).await.map_err(|e2| {
                        error!(target: "pairing", error = %e2, "Failed to reconnect lockdown after re-pair");
                        format!("Failed to start lockdown session: {} (re-pair also failed: {})", e, e2)
                    })?;
                    lc.start_session(&pairing_file).await.map_err(|e2| {
                        error!(target: "pairing", error = %e2, "Session still fails after re-pair");
                        format!(
                            "Failed to start session even after re-pairing device {} (uuid: {}): original error: {}, post-repair error: {}",
                            device.name, device.uuid, e, e2
                        )
                    })?;
                    info!(
                        target: "pairing",
                        provider_udid = %provider.udid,
                        "Lockdown session started after successful re-pair (was: {})", e
                    );
                }
                Err(repair_err) => {
                    return Err(format!(
                        "Failed to start lockdown session for device {} (uuid: {}, udid: {}): {}\n\n\
                        Automatic re-pair also failed: {}",
                        device.name, device.uuid, provider.udid, e, repair_err
                    ));
                }
            }
        }
    }

    lc.set_value(
        "EnableWifiDebugging",
        true.into(),
        Some("com.apple.mobile.wireless_lockdown"),
    )
    .await
    .map_err(|e| {
        error!(
            target: "pairing",
            device_name = %device.name,
            device_uuid = %device.uuid,
            provider_udid = %provider.udid,
            error = %e,
            "Failed to enable wifi debugging"
        );
        format!(
            "Failed to enable wifi debugging for device {} (uuid: {}, udid: {}): {}",
            device.name, device.uuid, provider.udid, e
        )
    })?;

    info!(
        target: "pairing",
        provider_udid = %provider.udid,
        "Pairing file retrieval completed"
    );

    Ok(pairing_file)
}

pub async fn place_pairing(
    device: DeviceInfo,
    bundle_id: String,
    path: String,
) -> Result<(), String> {
    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        bundle_id = %bundle_id,
        path = %path,
        "Starting pairing file placement"
    );

    let mut usbmuxd = UsbmuxdConnection::default()
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                error = %e,
                "Failed to connect to usbmuxd"
            );
            format!(
                "Failed to connect to usbmuxd for device {} (uuid: {}): {}",
                device.name, device.uuid, e
            )
        })?;

    let provider = get_provider_from_connection(&device, &mut usbmuxd).await?;
    debug!(
        target: "pairing",
        provider_udid = %provider.udid,
        "Resolved provider for place_pairing"
    );

    let pairing_file = pairing_file(device.clone(), &mut usbmuxd).await?;

    let house_arrest_client = HouseArrestClient::connect(&provider)
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                error = %e,
                "Failed to connect to house arrest"
            );
            format!(
                "Failed to connect to house arrest for device {} (uuid: {}, udid: {}): {}",
                device.name, device.uuid, provider.udid, e
            )
        })?;

    let mut afc_client = house_arrest_client
        .vend_documents(bundle_id)
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                error = %e,
                "Failed to vend app Documents directory"
            );
            format!(
                "Failed to vend app Documents directory for device {} (uuid: {}, udid: {}): {}",
                device.name, device.uuid, provider.udid, e
            )
        })?;

    let documents_dir = format!(
        "/Documents/{}",
        path.rsplit_once('/').map(|x| x.0).unwrap_or("")
    );
    let destination_path = format!("/Documents/{}", path);

    debug!(
        target: "pairing",
        documents_dir = %documents_dir,
        destination_path = %destination_path,
        "Preparing AFC destination paths"
    );

    afc_client
        .mk_dir(documents_dir.clone())
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                documents_dir = %documents_dir,
                error = %e,
                "Failed to create destination directory"
            );
            format!(
                "Failed to create Documents directory {} for device {} (uuid: {}, udid: {}): {}",
                documents_dir, device.name, device.uuid, provider.udid, e
            )
        })?;

    let mut file = afc_client
        .open(
            destination_path.clone(),
            idevice::afc::opcode::AfcFopenMode::Wr,
        )
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                destination_path = %destination_path,
                error = %e,
                "Failed to open destination pairing file"
            );
            format!(
                "Failed to open destination file {} on device {} (uuid: {}, udid: {}): {}",
                destination_path, device.name, device.uuid, provider.udid, e
            )
        })?;

    let serialized_pairing = pairing_file
        .serialize()
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                error = %e,
                "Failed to serialize pairing file"
            );
            format!(
                "Failed to serialize pairing file for device {} (uuid: {}, udid: {}): {}",
                device.name, device.uuid, provider.udid, e
            )
        })?;

    file.write_entire(&serialized_pairing)
    .await
    .map_err(|e| {
        error!(
            target: "pairing",
            device_name = %device.name,
            device_uuid = %device.uuid,
            provider_udid = %provider.udid,
            destination_path = %destination_path,
            bytes = serialized_pairing.len(),
            error = %e,
            "Failed to write pairing file"
        );
        format!(
            "Failed to write pairing file to {} for device {} (uuid: {}, udid: {}): {}",
            destination_path, device.name, device.uuid, provider.udid, e
        )
    })?;

    file.close()
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                provider_udid = %provider.udid,
                destination_path = %destination_path,
                error = %e,
                "Failed to close pairing file"
            );
            format!(
                "Failed to close destination file {} for device {} (uuid: {}, udid: {}): {}",
                destination_path, device.name, device.uuid, provider.udid, e
            )
        })?;

    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        provider_udid = %provider.udid,
        destination_path = %destination_path,
        "Pairing file placement completed"
    );

    Ok(())
}

#[tauri::command]
pub async fn place_pairing_cmd(
    device_state: State<'_, DeviceInfoMutex>,
    bundle_id: String,
    path: String,
) -> Result<(), String> {
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return Err("No device selected".to_string()),
        }
    };

    place_pairing(device, bundle_id, path).await
}

// prompt for a location to save the pairing file, then export it there. This is for advanced users who want to use the pairing file with other tools, or just want a backup of it. Normal users should use the "Place" button next to the app they want to pair with instead, which will transfer the pairing file automatically.
#[tauri::command]
pub async fn export_pairing_cmd(
    device_state: State<'_, DeviceInfoMutex>,
    app: AppHandle,
) -> Result<(), String> {
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return Err("No device selected".to_string()),
        }
    };

    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        "Starting pairing file export"
    );

    let pairing_file = {
        let mut usbmuxd = UsbmuxdConnection::default()
            .await
            .map_err(|e| {
                error!(
                    target: "pairing",
                    device_name = %device.name,
                    device_uuid = %device.uuid,
                    error = %e,
                    "Failed to connect to usbmuxd for export"
                );
                format!(
                    "Failed to connect to usbmuxd for device {} (uuid: {}): {}",
                    device.name, device.uuid, e
                )
            })?;

        pairing_file(device.clone(), &mut usbmuxd).await?
    };

    let save_path = app
        .dialog()
        .file()
        .add_filter("Pairing File", &["plist", "mobiledevicepairing"])
        .set_file_name("pairingFile.plist")
        .set_title("Export Pairing File")
        .blocking_save_file();

    if let Some(save_path) = save_path
        && let Some(save_path) = save_path.as_path()
    {
        debug!(
            target: "pairing",
            save_path = %save_path.display(),
            "Selected pairing export destination"
        );

        let serialized_pairing = pairing_file
            .serialize()
            .map_err(|e| {
                error!(
                    target: "pairing",
                    device_name = %device.name,
                    device_uuid = %device.uuid,
                    error = %e,
                    "Failed to serialize pairing file for export"
                );
                format!(
                    "Failed to serialize pairing file for device {} (uuid: {}): {}",
                    device.name, device.uuid, e
                )
            })?;

        tokio::fs::write(
            save_path,
            &serialized_pairing,
        )
        .await
        .map_err(|e| {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                save_path = %save_path.display(),
                bytes = serialized_pairing.len(),
                error = %e,
                "Failed to write pairing file to disk"
            );
            format!(
                "Failed to write pairing file to {} for device {} (uuid: {}): {}",
                save_path.display(), device.name, device.uuid, e
            )
        })?;

        info!(
            target: "pairing",
            device_name = %device.name,
            device_uuid = %device.uuid,
            save_path = %save_path.display(),
            "Pairing file export completed"
        );

        Ok(())
    } else {
        warn!(
            target: "pairing",
            device_name = %device.name,
            device_uuid = %device.uuid,
            "Pairing export cancelled by user"
        );
        Err("Save cancelled".to_string())
    }
}

#[tauri::command]
pub async fn repair_cmd(
    device_state: State<'_, DeviceInfoMutex>,
) -> Result<(), String> {
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return Err("No device selected".to_string()),
        }
    };

    let mut usbmuxd = UsbmuxdConnection::default().await.map_err(|e| {
        format!("Failed to connect to usbmuxd: {}", e)
    })?;

    repair_device(&device, &mut usbmuxd).await?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingAppInfo {
    pub name: String,
    pub bundle_id: String,
    pub path: String,
}

#[tauri::command]
pub async fn installed_pairing_apps(
    device_state: State<'_, DeviceInfoMutex>,
) -> Result<Vec<PairingAppInfo>, String> {
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return Err("No device selected".to_string()),
        }
    };

    info!(
        target: "pairing",
        device_name = %device.name,
        device_uuid = %device.uuid,
        "Loading installed pairing apps"
    );

    let provider = get_provider(&device).await?;
    let mut installation_proxy = match InstallationProxyClient::connect(&provider).await {
        Ok(proxy) => proxy,
        Err(IdeviceError::InvalidHostID) => {
            warn!(
                target: "pairing",
                device_name = %device.name,
                "Installation proxy InvalidHostID — attempting automatic re-pair"
            );
            let mut usbmuxd = UsbmuxdConnection::default().await.map_err(|e| {
                format!("Failed to connect to usbmuxd for re-pair: {}", e)
            })?;
            repair_device(&device, &mut usbmuxd).await?;
            // Retry after re-pair
            InstallationProxyClient::connect(&provider).await.map_err(|e| {
                format!("Failed to connect to installation proxy after re-pair: {}", e)
            })?
        }
        Err(IdeviceError::DeviceLocked) => {
            return Err("Failed to connect to installation proxy: device locked\n\n\
                DeviceLocked: unlock the device screen and try again.".to_string());
        }
        Err(e) => {
            error!(
                target: "pairing",
                device_name = %device.name,
                device_uuid = %device.uuid,
                error = %e,
                "Failed to connect to installation proxy"
            );
            return Err(format!("Failed to connect to installation proxy: {}", e));
        }
    };

    let installed_apps = installation_proxy
        .get_apps(Some("User"), None)
        .await
        .map_err(|e| format!("Failed to get installed apps: {}", e))?;

    let mut installed = HashMap::new();
    for (bundle_id, app) in installed_apps {
        let n = app
            .as_dictionary()
            .and_then(|x| x.get("CFBundleDisplayName").and_then(|x| x.as_string()))
            .ok_or("Failed to parse installed apps".to_string())?;

        if PAIRING_APPS.iter().any(|(name, _)| name == &n) {
            if bundle_id.contains("com.stik.stikdebug") {
                installed.insert(format!("{} (Sideloaded)", n), bundle_id);
            } else {
                installed.insert(n.to_string(), bundle_id);
            }
        }
    }

    let mut result = Vec::new();
    for (name, path) in PAIRING_APPS {
        if let Some(bundle_id) = installed.get(*name) {
            result.push(PairingAppInfo {
                name: name.to_string(),
                bundle_id: bundle_id.to_string(),
                path: path.to_string(),
            });
        }
    }
    Ok(result)
}

pub async fn get_sidestore_info(
    device: DeviceInfo,
    live_container: bool,
) -> Result<Option<PairingAppInfo>, String> {
    let provider = get_provider(&device).await?;
    let mut installation_proxy = match InstallationProxyClient::connect(&provider).await {
        Ok(proxy) => proxy,
        Err(IdeviceError::InvalidHostID) => {
            warn!(
                target: "pairing",
                device_name = %device.name,
                "Installation proxy InvalidHostID in get_sidestore_info — attempting automatic re-pair"
            );
            let mut usbmuxd = UsbmuxdConnection::default().await.map_err(|e| {
                format!("Failed to connect to usbmuxd for re-pair: {}", e)
            })?;
            repair_device(&device, &mut usbmuxd).await?;
            InstallationProxyClient::connect(&provider).await.map_err(|e| {
                format!("Failed to connect to installation proxy after re-pair: {}", e)
            })?
        }
        Err(IdeviceError::DeviceLocked) => {
            return Err("Failed to connect to installation proxy: device locked\n\n\
                DeviceLocked: unlock the device screen and try again.".to_string());
        }
        Err(e) => {
            return Err(format!("Failed to connect to installation proxy: {}", e));
        }
    };

    let installed_apps = installation_proxy
        .get_apps(Some("User"), None)
        .await
        .map_err(|e| format!("Failed to get installed apps: {}", e))?;

    for (bundle_id, app) in installed_apps {
        let n = app
            .as_dictionary()
            .and_then(|x| x.get("CFBundleDisplayName").and_then(|x| x.as_string()))
            .ok_or("Failed to parse installed apps".to_string())?;

        if n == "SideStore" || (live_container && n == "LiveContainer") {
            return Ok(Some(PairingAppInfo {
                name: n.to_string(),
                bundle_id: bundle_id.to_string(),
                path: PAIRING_APPS
                    .iter()
                    .find(|(name, _)| name == &n)
                    .map(|(_, path)| path.to_string())
                    .unwrap_or_default(),
            }));
        }
    }

    Ok(None)
}
