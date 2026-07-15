use std::{path::PathBuf, sync::Mutex};

use crate::{
    device::{
        DeviceInfoMutex, DeviceInfoWithPairing, DeviceTransport, get_provider,
        get_provider_from_connection, get_usbmuxd,
    },
    error::AppError,
    operation::Operation,
    pairing::{get_sidestore_info, place_file},
    vision,
};
use isideload::dev::devices::DevicesApi;
use isideload::sideload::{application::SpecialApp, sideloader::Sideloader};
use tauri::{AppHandle, Manager, State, Window};

/// The RP pairing file name a Vision Pro build of SideStore reads on boot (its
/// AppBootManager prefers this over the classic usbmux pairing).
const VISION_PAIRING_FILENAME: &str = "rp_pairing_file.plist";

/// Vision Pro build of SideStore: patched with visionOS support (device-class
/// registration tolerance, RemotePairing boot preference, arm64). Tracks the
/// rebelancap/SideStore fork until the fixes land upstream. There is no nightly or
/// LiveContainer visionOS build, so a Vision Pro always installs this.
const SIDESTORE_VP_URL: &str =
    "https://github.com/rebelancap/SideStore/releases/download/visionos-0.6.4/SideStore-visionOS.ipa";

/// LiveContainer with the visionOS-patched SideStore embedded (built by
/// rebelancap/LiveContainer's CI from the patched LiveContainer/SideStore). The
/// embedded SideStore reads its pairing from `SideStore/Documents/` inside the
/// LiveContainer container.
const LIVECONTAINER_VP_URL: &str =
    "https://github.com/rebelancap/LiveContainer/releases/download/visionos/LiveContainer-SideStore-visionOS.ipa";

/// Where the LiveContainer-embedded SideStore looks for its RP pairing file (relative
/// to the LiveContainer container's Documents), mirroring the classic
/// `SideStore/Documents/ALTPairingFile.mobiledevicepairing` iOS path.
const LIVECONTAINER_VISION_PAIRING_PATH: &str = "SideStore/Documents/rp_pairing_file.plist";

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

    // Vision Pro isn't a usbmux device: sign with the Apple account (transport-agnostic),
    // then install the signed bundle over the RP tunnel instead of isideload's usbmux
    // install path.
    if device.info.transport == DeviceTransport::Vision {
        return sideload_vision(device, sideloader_state, app_path).await;
    }

    let provider = get_provider(&device.info).await?;

    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    let special = sideloader
        .get_mut()
        .install_app(&provider, app_path.into(), false)
        .await?;

    Ok(special)
}

/// Sign + install onto a Vision Pro over Wi-Fi. Signing reuses isideload's account
/// flow (it only talks to Apple + the local filesystem); only registration needs the
/// device UDID, and only the install runs over the RP tunnel.
async fn sideload_vision(
    device: DeviceInfoWithPairing,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
) -> Result<Option<SpecialApp>, AppError> {
    let ip = device
        .info
        .ip
        .clone()
        .ok_or_else(|| AppError::RemotePairing("Vision Pro has no IP address".into()))?;

    // The UDID is needed to register the device with the developer account. Prefer the
    // one captured at selection; otherwise read it over a short-lived tunnel now.
    let udid = if device.info.udid.is_empty() {
        vision::read_udid(&ip, &device.pairing).await?
    } else {
        device.info.udid.clone()
    };

    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;
    let team = sideloader.get_mut().get_team().await?;
    sideloader
        .get_mut()
        .get_dev_session()
        .ensure_device_registered(&team, &device.info.name, &udid, None)
        .await?;

    let (signed_path, special) = sideloader
        .get_mut()
        .sign_app(app_path.into(), Some(team), false)
        .await?;

    // Fresh tunnel for the install itself (signing above is network-bound and could
    // otherwise idle out an earlier tunnel).
    let mut session = vision::VisionSession::connect(&ip, &device.pairing).await?;
    vision::install_app(&mut session, &signed_path, |pct| {
        tracing::info!("Installing to Vision Pro: {pct}%");
    })
    .await?;

    Ok(special)
}

#[tauri::command]
pub async fn sideload_operation(
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
) -> Result<(), AppError> {
    let op = Operation::new("sideload".to_string(), &window);
    op.start("install")?;
    op.fail_if_err(
        "install",
        sideload(device_state, sideloader_state, app_path).await,
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
    let op = Operation::new("install_sidestore".to_string(), &window);
    op.start("download")?;

    // A Vision Pro needs a patched arm64 visionOS build; there's no visionOS *nightly*,
    // so nightly is routed to the same patched build, while LiveContainer has its own
    // patched visionOS build (LiveContainer + the visionOS-patched SideStore embedded).
    // iPhone/iPad keep the upstream SideStore builds.
    let is_vision = {
        let guard = device_state.lock().unwrap();
        matches!(
            guard.as_ref().map(|d| d.info.transport),
            Some(DeviceTransport::Vision)
        )
    };

    // TODO: Cache & check version to avoid re-downloading
    let (filename, url) = if is_vision {
        if live_container {
            ("LiveContainer-SideStore-visionOS.ipa", LIVECONTAINER_VP_URL)
        } else {
            ("SideStore-visionOS.ipa", SIDESTORE_VP_URL)
        }
    } else if live_container {
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
            device_state,
            sideloader_state,
            dest.to_string_lossy().to_string(),
        )
        .await,
    )?;
    op.move_on("install", "pairing")?;
    if device.info.transport == DeviceTransport::Vision {
        // Place the RP pairing file into SideStore over the tunnel so it can reach the
        // device on its own (SideStore-on-visionOS boots from rp_pairing_file.plist).
        let ip = match device.info.ip.clone() {
            Some(ip) => ip,
            None => {
                return op.fail(
                    "pairing",
                    AppError::RemotePairing("Vision Pro has no IP address".into()),
                );
            }
        };
        let mut session =
            op.fail_if_err("pairing", vision::VisionSession::connect(&ip, &device.pairing).await)?;
        // LiveContainer runs SideStore as a guest, so its pairing lives under
        // SideStore/Documents/ inside the LiveContainer container; a plain SideStore
        // install reads rp_pairing_file.plist straight from its own Documents.
        let (needle, path) = if live_container {
            ("livecontainer", LIVECONTAINER_VISION_PAIRING_PATH)
        } else {
            ("sidestore", VISION_PAIRING_FILENAME)
        };
        let bundle = op.fail_if_err("pairing", vision::find_app(&mut session, needle).await)?;
        op.fail_if_err(
            "pairing",
            vision::place_into(&mut session, &bundle, path, &device.pairing).await,
        )?;
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
