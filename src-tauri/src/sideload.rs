use std::{path::PathBuf, sync::Mutex};

use crate::{
    device::{DeviceInfoMutex, get_provider, get_provider_from_connection, get_usbmuxd},
    error::AppError,
    operation::Operation,
    pairing::{PairingAppInfo, get_sidestore_info, place_file},
    wifi_rsd::open_rsd_tunnel,
};
use idevice::{
    RsdService,
    afc::opcode::AfcFopenMode,
    house_arrest::HouseArrestClient,
    installation_proxy::InstallationProxyClient,
    rsd::RsdHandshake,
    tcp::handle::AdapterHandle,
};
use isideload::{
    dev::{device_type::DeveloperDeviceType, devices::DevicesApi},
    sideload::{application::SpecialApp, install::install_app_rsd, sideloader::Sideloader},
};
use tauri::{AppHandle, Manager, State, Window};
use tracing::{info, warn};

pub type SideloaderMutex = Mutex<Option<Sideloader>>;

pub struct SideloaderGuard<'a> {
    state: &'a SideloaderMutex,
    sideloader: Option<Sideloader>,
}

impl<'a> SideloaderGuard<'a> {
    pub fn take(state: &'a SideloaderMutex) -> Result<Self, AppError> {
        let mut guard = state.lock().unwrap();
        let sideloader = guard.take().ok_or(AppError::NotLoggedIn)?;
        Ok(Self {
            state,
            sideloader: Some(sideloader),
        })
    }

    pub fn get_mut(&mut self) -> &mut Sideloader {
        self.sideloader
            .as_mut()
            .expect("Sideloader should be present")
    }
}

impl Drop for SideloaderGuard<'_> {
    fn drop(&mut self) {
        let mut guard = self.state.lock().unwrap();
        *guard = self.sideloader.take();
    }
}

pub async fn sideload(
    handle: &AppHandle,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
) -> Result<Option<SpecialApp>, AppError> {
    let device = {
        let device_lock = device_state.lock().unwrap();
        match &*device_lock {
            Some(d) => d.clone(),
            None => return Err(AppError::NoDeviceSelected),
        }
    };

    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    if device.info.connection_type == "Network" && device.info.network_address.is_some() {
        info!("Installing {} over RemotePairing/RSD", device.info.udid);

        let (mut rsd_provider, mut handshake) = open_rsd_tunnel(handle, &device.info).await?;

        let team = sideloader.get_mut().get_team().await?;

        sideloader
            .get_mut()
            .get_dev_session()
            .ensure_device_registered(
                &team,
                &device.info.name,
                &device.info.udid,
                None::<DeveloperDeviceType>,
            )
            .await?;

        let (signed_app_path, special) = sideloader
            .get_mut()
            .sign_app(
                app_path.clone().into(),
                Some(team),
                false,
                None::<fn(f32) -> std::future::Ready<()>>,
            )
            .await?;

        install_app_rsd(
            &mut rsd_provider,
            &mut handshake,
            &signed_app_path,
            |progress| {
                info!("Installing over RSD: {}%", progress);
            },
        )
        .await?;

        if let Err(e) = tokio::fs::remove_dir_all(&signed_app_path).await {
            warn!(
                "Failed to remove temporary RSD-signed app directory {}: {}",
                signed_app_path.display(),
                e
            );
        }

        return Ok(special);
    }

    let provider = get_provider(&device.info).await?;

    let special = sideloader
        .get_mut()
        .install_app(
            &provider,
            app_path.into(),
            false,
            None::<fn(f32) -> std::future::Ready<()>>,
        )
        .await?;

    Ok(special)
}

async fn get_sidestore_info_rsd(
    provider: &mut AdapterHandle,
    handshake: &mut RsdHandshake,
    live_container: bool,
) -> Result<Option<PairingAppInfo>, AppError> {
    let mut installation_proxy = InstallationProxyClient::connect_rsd(provider, handshake)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                "Failed to connect to installation proxy over RSD".into(),
                e.to_string(),
            )
        })?;

    let installed_apps = installation_proxy
        .get_apps(Some("User"), None)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                "Failed to get installed apps over RSD".into(),
                e.to_string(),
            )
        })?;

    for (bundle_id, app) in installed_apps {
        let name = app
            .as_dictionary()
            .and_then(|x| x.get("CFBundleDisplayName").and_then(|x| x.as_string()))
            .ok_or(AppError::Misc("Failed to parse installed apps".to_string()))?;

        if name == "SideStore" || (live_container && name == "LiveContainer") {
            let path = if name == "LiveContainer" {
                "SideStore/Documents/ALTPairingFile.mobiledevicepairing"
            } else {
                "ALTPairingFile.mobiledevicepairing"
            };

            return Ok(Some(PairingAppInfo {
                name: name.to_string(),
                bundle_id,
                path: path.to_string(),
            }));
        }
    }

    Ok(None)
}

async fn place_file_rsd(
    pairing: Vec<u8>,
    provider: &mut AdapterHandle,
    handshake: &mut RsdHandshake,
    bundle_id: String,
    path: String,
) -> Result<(), AppError> {
    let house_arrest_client = HouseArrestClient::connect_rsd(provider, handshake)
        .await
        .map_err(|e| {
            AppError::HouseArrest(
                "Failed to connect to house arrest over RSD".into(),
                e.to_string(),
            )
        })?;

    let mut afc_client = house_arrest_client
        .vend_documents(bundle_id)
        .await
        .map_err(|e| AppError::HouseArrest("Failed to vend documents".into(), e.to_string()))?;

    afc_client
        .mk_dir(format!(
            "/Documents/{}",
            path.rsplit_once('/').map(|x| x.0).unwrap_or("")
        ))
        .await
        .map_err(|e| {
            AppError::HouseArrest("Failed to create Documents directory".into(), e.to_string())
        })?;

    let mut file = afc_client
        .open(format!("/Documents/{}", path), AfcFopenMode::Wr)
        .await
        .map_err(|e| {
            AppError::HouseArrest("Failed to open file on device".into(), e.to_string())
        })?;

    file.write_entire(&pairing)
        .await
        .map_err(|e| AppError::HouseArrest("Failed to write pairing file".into(), e.to_string()))?;
    file.close()
        .await
        .map_err(|e| AppError::HouseArrest("Failed to close file".into(), e.to_string()))?;

    Ok(())
}

#[tauri::command]
pub async fn sideload_operation(
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
) -> Result<(), AppError> {
    Box::pin(sideload_operation_impl(
        window,
        device_state,
        sideloader_state,
        app_path,
    ))
    .await
}

async fn sideload_operation_impl(
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
) -> Result<(), AppError> {
    let op = Operation::new("sideload".to_string(), &window);
    op.start("install")?;
    op.fail_if_err(
        "install",
        sideload(
            window.app_handle(),
            device_state,
            sideloader_state,
            app_path,
        )
        .await,
    )?;
    op.complete("install")?;
    Ok(())
}

#[tauri::command]
pub async fn install_sidestore_operation(
    handle: AppHandle,
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    nightly: bool,
    live_container: bool,
) -> Result<(), AppError> {
    Box::pin(install_sidestore_operation_impl(
        handle,
        window,
        device_state,
        sideloader_state,
        nightly,
        live_container,
    ))
    .await
}

async fn install_sidestore_operation_impl(
    handle: AppHandle,
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    nightly: bool,
    live_container: bool,
) -> Result<(), AppError> {
    let op = Operation::new("install_sidestore".to_string(), &window);
    op.start("download")?;
    // TODO: Cache & check version to avoid re-downloading
    let (filename, url) = if live_container {
        if nightly {
            (
                "LiveContainerSideStore-Nightly.ipa",
                "https://github.com/LiveContainer/LiveContainer/releases/download/nightly/LiveContainer+SideStore.ipa",
            )
        } else {
            (
                "LiveContainerSideStore.ipa",
                "https://github.com/LiveContainer/LiveContainer/releases/latest/download/LiveContainer+SideStore.ipa",
            )
        }
    } else if nightly {
        (
            "SideStore-Nightly.ipa",
            "https://github.com/SideStore/SideStore/releases/download/nightly/SideStore.ipa",
        )
    } else {
        (
            "SideStore.ipa",
            "https://github.com/SideStore/SideStore/releases/latest/download/SideStore.ipa",
        )
    };

    let dest = handle
        .path()
        .temp_dir()
        .map_err(|e| AppError::Filesystem("Failed to get temp dir".into(), e.to_string()))?
        .join(filename);
    op.fail_if_err("download", download(url, &dest).await)?;
    op.move_on("download", "install")?;
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return op.fail("install", AppError::NoDeviceSelected),
        }
    };
    op.fail_if_err(
        "install",
        sideload(
            &handle,
            device_state,
            sideloader_state,
            dest.to_string_lossy().to_string(),
        )
        .await,
    )?;
    op.move_on("install", "pairing")?;

    if device.info.connection_type == "Network" && device.info.network_address.is_some() {
        info!(
            "Placing SideStore pairing file over RemotePairing/RSD for {}",
            device.info.udid
        );

        let (mut rsd_provider, mut handshake) =
            op.fail_if_err("pairing", open_rsd_tunnel(&handle, &device.info).await)?;

        let sidestore_info = op.fail_if_err(
            "pairing",
            get_sidestore_info_rsd(&mut rsd_provider, &mut handshake, live_container).await,
        )?;

        if let Some(info) = sidestore_info {
            op.fail_if_err(
                "pairing",
                place_file_rsd(
                    device.pairing,
                    &mut rsd_provider,
                    &mut handshake,
                    info.bundle_id,
                    info.path,
                )
                .await,
            )?;
        } else {
            return op.fail(
                "pairing",
                AppError::HouseArrest(
                    "SideStore's not found".into(),
                    "The device did not report SideStore's bundle ID as installed".into(),
                ),
            );
        }
    } else {
        let sidestore_info = op.fail_if_err(
            "pairing",
            get_sidestore_info(&device.info, live_container).await,
        )?;

        if let Some(info) = sidestore_info {
            let mut usbmuxd = op.fail_if_err("pairing", get_usbmuxd().await)?;

            let provider = op.fail_if_err(
                "pairing",
                get_provider_from_connection(&device.info, &mut usbmuxd).await,
            )?;

            op.fail_if_err(
                "pairing",
                place_file(device.pairing, &provider, info.bundle_id, info.path).await,
            )?;
        } else {
            return op.fail(
                "pairing",
                AppError::HouseArrest(
                    "SideStore's not found".into(),
                    "The device did not report SideStore's bundle ID as installed".into(),
                ),
            );
        }
    }

    op.complete("pairing")?;
    Ok(())
}

pub async fn download(url: impl AsRef<str>, dest: &PathBuf) -> Result<(), AppError> {
    let response = reqwest::get(url.as_ref())
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    if !response.status().is_success() {
        return Err(AppError::Download(format!(
            "Failed to download file: HTTP {}",
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    tokio::fs::write(dest, &bytes).await.map_err(|e| {
        AppError::Filesystem("Failed to write downloaded file".into(), e.to_string())
    })?;

    Ok(())
}
