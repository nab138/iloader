use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

// used https://github.com/jkcoxson/idevice_pair/ as a guide
use idevice::{
    IdeviceError, IdeviceService, RemoteXpcClient,
    core_device_proxy::CoreDeviceProxy,
    house_arrest::HouseArrestClient,
    installation_proxy::InstallationProxyClient,
    lockdown::LockdownClient,
    provider::IdeviceProvider,
    remote_pairing::{RemotePairingClient, RpPairingFile},
    rsd::RsdHandshake,
    usbmuxd::UsbmuxdConnection,
};
use isideload::util::storage::{InMemoryStorage, SideloadingStorage};
use plist_macro::{plist, plist_to_xml_bytes};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    device::{DeviceInfo, DeviceInfoMutex, get_provider, get_provider_from_connection},
    secure_storage::{create_sideloading_storage, keyring_available},
};

struct PairingStorageEntry {
    keyring_enabled: bool,
    storage: Box<dyn SideloadingStorage>,
}

static PAIRING_STORAGE: OnceLock<Mutex<PairingStorageEntry>> = OnceLock::new();

const PAIRING_APPS: &[(&str, &str)] = &[
    ("SideStore", "ALTPairingFile.mobiledevicepairing"),
    (
        "LiveContainer",
        "SideStore/Documents/ALTPairingFile.mobiledevicepairing",
    ),
    ("Feather", "pairingFile.plist"),
    ("StikDebug", "pairingFile.plist"),
    ("StikDebug (Sideloaded)", "rp_pairing_file.plist"),
    ("StikTest", "stiktest_pairing.plist"),
    ("Protokolle", "pairingFile.plist"),
    ("Antrag", "pairingFile.plist"),
    ("SparseBox", "pairingFile.plist"),
    ("StikStore", "pairingFile.plist"),
    ("ByeTunes", "pairing file/pairingFile.plist"),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingAppInfo {
    pub name: String,
    pub bundle_id: String,
    pub path: String,
}

async fn generate_lockdown_plist(
    device: &DeviceInfo,
    provider: &dyn IdeviceProvider,
    usbmuxd: &mut UsbmuxdConnection,
) -> Result<plist::Value, String> {
    let mut pairing_file = usbmuxd.get_pair_record(&device.udid).await.map_err(|e| {
        format!(
            "Failed to get pairing record for device {}: {}",
            device.name, e
        )
    })?;

    pairing_file.udid = Some(device.udid.clone());

    let mut lc = LockdownClient::connect(provider)
        .await
        .map_err(|e| format!("Failed to connect to lockdown: {}", e))?;

    lc.start_session(&pairing_file)
        .await
        .map_err(|e| format!("Failed to start lockdown session: {}", e))?;

    lc.set_value(
        "EnableWifiDebugging",
        true.into(),
        Some("com.apple.mobile.wireless_lockdown"),
    )
    .await
    .map_err(|e| format!("Failed to enable wifi debugging: {}", e))?;

    plist::Value::from_reader_xml(std::io::Cursor::new(
        pairing_file
            .serialize()
            .map_err(|e| format!("Failed to serialize pairing file: {}", e))?,
    ))
    .map_err(|e| format!("Failed to parse pairing file as plist: {}", e))
}

async fn generate_rppairing_plist(
    provider: &dyn IdeviceProvider,
) -> Result<(plist::Value, Vec<u8>), IdeviceError> {
    let bytes = generate_rppairing(provider, "iloader").await?.to_bytes();
    let plist = plist::Value::from_reader_xml(std::io::Cursor::new(&bytes))
        .map_err(|e| IdeviceError::InternalError(format!("Invalid RPPairing plist: {}", e)))?;
    Ok((plist, bytes))
}

async fn generate_rppairing(
    provider: &dyn IdeviceProvider,
    hostname: &str,
) -> Result<RpPairingFile, IdeviceError> {
    info!("Connecting to CoreDeviceProxy...");
    let proxy = CoreDeviceProxy::connect(provider).await?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    info!("CDTunnel established, RSD port {rsd_port}");

    info!("Starting TCP stack...");
    let adapter = proxy.create_software_tunnel()?;
    let mut adapter = adapter.to_async_handle();

    info!("Performing RSD handshake...");
    let rsd_stream = adapter.connect(rsd_port).await?;
    let handshake = RsdHandshake::new(rsd_stream).await?;
    info!("RSD: {} services", handshake.services.len());
    let tunnel_service = handshake
        .services
        .get("com.apple.internal.dt.coredevice.untrusted.tunnelservice")
        .ok_or_else(|| IdeviceError::InternalError("Untrusted tunnel service not found".into()))?;

    info!("Connecting to untrusted tunnel service...");
    let tunnel_service_stream = adapter.connect(tunnel_service.port).await?;
    let mut remote_xpc = RemoteXpcClient::new(tunnel_service_stream).await?;
    remote_xpc.do_handshake().await?;
    let _ = remote_xpc.recv_root().await;

    info!("Starting RPPairing...");
    info!("(You may need to tap Trust on the device)");
    let mut pairing_file = RpPairingFile::generate(hostname);
    let mut pairing_client = RemotePairingClient::new(remote_xpc, hostname, &mut pairing_file);
    pairing_client
        .connect(async |_| "000000".to_string(), ())
        .await?;

    Ok(pairing_file)
}

pub async fn place_file(
    pairing: Vec<u8>,
    provider: &dyn IdeviceProvider,
    bundle_id: String,
    path: String,
) -> Result<(), String> {
    let house_arrest_client = HouseArrestClient::connect(provider)
        .await
        .map_err(|e| format!("Failed to connect to house arrest: {}", e))?;

    let mut afc_client = house_arrest_client
        .vend_documents(bundle_id)
        .await
        .map_err(|e| format!("Failed to vend documents: {}", e))?;

    afc_client
        .mk_dir(format!(
            "/Documents/{}",
            path.rsplit_once('/').map(|x| x.0).unwrap_or("")
        ))
        .await
        .map_err(|e| format!("Failed to create Documents directory: {}", e))?;

    let mut file = afc_client
        .open(
            format!("/Documents/{}", path),
            idevice::afc::opcode::AfcFopenMode::Wr,
        )
        .await
        .map_err(|e| format!("Failed to open file on device: {}", e))?;

    file.write_entire(&pairing)
        .await
        .map_err(|e| format!("Failed to write pairing file: {}", e))?;
    file.close()
        .await
        .map_err(|e| format!("Failed to close file: {}", e))?;

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

    let mut usbmuxd = UsbmuxdConnection::default()
        .await
        .map_err(|e| format!("Failed to connect to usbmuxd: {}", e))?;

    let provider = get_provider_from_connection(&device.info, &mut usbmuxd).await?;

    place_file(device.pairing, &provider, bundle_id, path).await
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
        tokio::fs::write(save_path, &device.pairing)
            .await
            .map_err(|e| format!("Failed to write pairing file: {}", e))?;

        Ok(())
    } else {
        Err("Save cancelled".to_string())
    }
}

fn build_pairing_storage_entry(app: &AppHandle, keyring_enabled: bool) -> PairingStorageEntry {
    let storage = create_sideloading_storage(app).unwrap_or_else(|e| {
        error!(
            "Failed to create sideloading storage, storing pairing file in memory: {}",
            e
        );
        Box::new(InMemoryStorage::new())
    });

    PairingStorageEntry {
        keyring_enabled,
        storage,
    }
}

fn with_pairing_storage<T>(
    app: &AppHandle,
    f: impl FnOnce(&dyn SideloadingStorage) -> Result<T, String>,
) -> Result<T, String> {
    let current_keyring_enabled = keyring_available();
    let storage = PAIRING_STORAGE
        .get_or_init(|| Mutex::new(build_pairing_storage_entry(app, current_keyring_enabled)));

    let mut guard = storage
        .lock()
        .map_err(|_| "Failed to lock pairing storage".to_string())?;

    if guard.keyring_enabled != current_keyring_enabled {
        info!(
            "Pairing storage backend changed at runtime (keyring_enabled: {} -> {}), recreating storage",
            guard.keyring_enabled, current_keyring_enabled
        );
        *guard = build_pairing_storage_entry(app, current_keyring_enabled);
    }

    f(guard.storage.as_ref())
}

pub async fn pairing_file(
    app: &AppHandle,
    device: &DeviceInfo,
    usbmuxd: &mut UsbmuxdConnection,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    let provider = get_provider(device).await?;

    let lockdown_plist = tokio::select! {
        _ = cancel.cancelled() => {
            return Err("Pairing cancelled".to_string());
        }
        res = generate_lockdown_plist(device, &provider, usbmuxd) => res?
    };

    let cache_key = format!("rppairing_file_{}", device.udid);

    let cached_rppairing = with_pairing_storage(app, |storage| {
        storage.retrieve_data(&cache_key).map_err(|e| {
            format!(
                "Failed to get RPPairing from storage for device {}: {}",
                device.name, e
            )
        })
    })?;

    let rppairing_plist = if let Some(cached) = cached_rppairing {
        match plist::Value::from_reader_xml(std::io::Cursor::new(&cached)) {
            Ok(plist) => plist,
            Err(e) => {
                warn!(
                    "Cached RPPairing is invalid for device {}, regenerating: {}",
                    device.name, e
                );

                let (generated_plist, generated_bytes) = tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err("Pairing cancelled".to_string());
                    }
                    res = generate_rppairing_plist(&provider) => {
                        res.map_err(|e| format!("Failed to generate RPPairing: {}", e))?
                    }
                };

                with_pairing_storage(app, |storage| {
                    storage
                        .store_data(&cache_key, &generated_bytes)
                        .map_err(|e| {
                            format!(
                                "Failed to store RPPairing for device {}: {}",
                                device.name, e
                            )
                        })
                })?;

                generated_plist
            }
        }
    } else {
        info!("Generating new RPPairing for device {}", device.name);

        let (generated_plist, generated_bytes) = tokio::select! {
            _ = cancel.cancelled() => {
                return Err("Pairing cancelled".to_string());
            }
            res = generate_rppairing_plist(&provider) => {
                res.map_err(|e| format!("Failed to generate RPPairing: {}", e))?
            }
        };

        with_pairing_storage(app, |storage| {
            storage
                .store_data(&cache_key, &generated_bytes)
                .map_err(|e| {
                    format!(
                        "Failed to store RPPairing for device {}: {}",
                        device.name, e
                    )
                })
        })?;

        generated_plist
    };

    if cancel.is_cancelled() {
        return Err("Pairing cancelled".to_string());
    }

    let pairing_plist = plist!(dict {
        :< lockdown_plist,
        :< rppairing_plist,
    });

    Ok(plist_to_xml_bytes(&pairing_plist))
}

#[tauri::command]
pub async fn delete_stored_rppairing(
    device_state: State<'_, DeviceInfoMutex>,
    app: AppHandle,
) -> Result<bool, String> {
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return Err("No device selected".to_string()),
        }
    };

    let cache_key = format!("rppairing_file_{}", device.info.udid);

    let had_pairing = with_pairing_storage(&app, |storage| {
        storage
            .retrieve_data(&cache_key)
            .map_err(|e| {
                format!(
                    "Failed to check stored RPPairing for device {}: {}",
                    device.info.name, e
                )
            })
            .map(|data| data.is_some_and(|bytes| !bytes.is_empty()))
    })?;

    with_pairing_storage(&app, |storage| {
        storage.delete(&cache_key).map_err(|e| {
            format!(
                "Failed to delete stored RPPairing for device {}: {}",
                device.info.name, e
            )
        })
    })?;

    Ok(had_pairing)
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
    let provider = get_provider(&device.info).await?;
    let mut installation_proxy = InstallationProxyClient::connect(&provider)
        .await
        .map_err(|e| format!("Failed to connect to installation proxy: {}", e))?;

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
    device: &DeviceInfo,
    live_container: bool,
) -> Result<Option<PairingAppInfo>, String> {
    let provider = get_provider(device).await?;
    let mut installation_proxy = InstallationProxyClient::connect(&provider)
        .await
        .map_err(|e| format!("Failed to connect to installation proxy: {}", e))?;

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
