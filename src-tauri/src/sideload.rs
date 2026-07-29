use std::{collections::HashSet, path::{Path, PathBuf}, sync::Mutex};
use futures::StreamExt;
use tokio::{io::AsyncWriteExt, time::{Duration, Instant, interval}};

use crate::{
    device::{get_provider, get_provider_from_connection, get_usbmuxd, DeviceInfoMutex},
    error::AppError,
    operation::Operation,
    pairing::{get_sidestore_info, place_file},
};
use idevice::{
    IdeviceService,
    afc::{AfcClient, opcode::AfcFopenMode},
    installation_proxy::InstallationProxyClient,
};
use isideload::{
    dev::developer_session::DevicesApi,
    sideload::{application::SpecialApp, sideloader::Sideloader},
    util::device::IdeviceInfo,
};
use plist_macro::plist;
use tauri::{AppHandle, Manager, State, Window};

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

fn calculate_total_size(path: &Path) -> Result<u64, AppError> {
    if path.is_file() {
        return std::fs::metadata(path)
            .map(|m| m.len())
            .map_err(|e| AppError::Filesystem("Failed to read file metadata".into(), e.to_string()));
    }

    if path.is_dir() {
        let mut total = 0_u64;
        let entries = std::fs::read_dir(path)
            .map_err(|e| AppError::Filesystem("Failed to read directory".into(), e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                AppError::Filesystem("Failed to read directory entry".into(), e.to_string())
            })?;
            total += calculate_total_size(&entry.path())?;
        }
        return Ok(total);
    }

    Ok(0)
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), AppError> {
    if root.is_file() {
        out.push(root.to_path_buf());
        return Ok(());
    }

    if root.is_dir() {
        let entries = std::fs::read_dir(root)
            .map_err(|e| AppError::Filesystem("Failed to read directory".into(), e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                AppError::Filesystem("Failed to read directory entry".into(), e.to_string())
            })?;
            collect_files(&entry.path(), out)?;
        }
    }

    Ok(())
}

async fn ensure_remote_dirs(
    afc_client: &mut AfcClient,
    remote_root: &str,
    rel_parent: &Path,
    created_dirs: &mut HashSet<String>,
) -> Result<(), AppError> {
    let mut current = remote_root.to_string();

    for comp in rel_parent.components() {
        let part = comp.as_os_str().to_string_lossy().replace('\\', "/");
        if part.is_empty() {
            continue;
        }
        current = format!("{}/{}", current, part);
        if created_dirs.insert(current.clone()) {
            afc_client
                .mk_dir(&current)
                .await
                .map_err(|e| AppError::DeviceComs(e.to_string()))?;
        }
    }

    Ok(())
}

async fn upload_signed_bundle(
    provider: &impl idevice::provider::IdeviceProvider,
    signed_app_path: &Path,
    mut on_upload_progress: impl FnMut(u64, u64),
) -> Result<(), AppError> {
    let mut afc_client = AfcClient::connect(provider)
        .await
        .map_err(|e| AppError::DeviceComs(e.to_string()))?;

    let remote_root = format!(
        "PublicStaging/{}",
        signed_app_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "App.app".to_string())
    );

    afc_client
        .mk_dir(&remote_root)
        .await
        .map_err(|e| AppError::DeviceComs(e.to_string()))?;

    let mut files = Vec::new();
    collect_files(signed_app_path, &mut files)?;

    let total_bytes = calculate_total_size(signed_app_path)?.max(1);
    let mut uploaded = 0_u64;
    let mut created_dirs = HashSet::new();
    created_dirs.insert(remote_root.clone());

    for file in files {
        let rel = file.strip_prefix(signed_app_path).map_err(|e| {
            AppError::Filesystem(
                "Failed to build relative upload path".into(),
                e.to_string(),
            )
        })?;

        let rel_parent = rel.parent().unwrap_or(Path::new(""));
        ensure_remote_dirs(&mut afc_client, &remote_root, rel_parent, &mut created_dirs).await?;

        let rel_norm = rel.to_string_lossy().replace('\\', "/");
        let remote_file = format!("{}/{}", remote_root, rel_norm);

        let mut file_handle = afc_client
            .open(remote_file, AfcFopenMode::WrOnly)
            .await
            .map_err(|e| AppError::DeviceComs(e.to_string()))?;

        let bytes = std::fs::read(&file)
            .map_err(|e| AppError::Filesystem("Failed to read local file".into(), e.to_string()))?;

        file_handle
            .write_entire(&bytes)
            .await
            .map_err(|e| AppError::DeviceComs(e.to_string()))?;

        file_handle
            .close()
            .await
            .map_err(|e| AppError::DeviceComs(e.to_string()))?;

        uploaded += bytes.len() as u64;
        on_upload_progress(uploaded, total_bytes);
    }

    Ok(())
}

async fn install_signed_bundle(
    provider: &impl idevice::provider::IdeviceProvider,
    signed_app_path: &Path,
    on_install_progress: impl Fn(f64),
) -> Result<(), AppError> {
    let mut instproxy_client = InstallationProxyClient::connect(provider)
        .await
        .map_err(|e| AppError::DeviceComs(e.to_string()))?;

    let remote_root = format!(
        "PublicStaging/{}",
        signed_app_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "App.app".to_string())
    );

    let options = plist!(dict {
        "PackageType": "Developer"
    });

    instproxy_client
        .install_with_callback(
            remote_root,
            Some(plist::Value::Dictionary(options)),
            |(percentage, _)| {
                on_install_progress((percentage as f64 / 100.0).clamp(0.0, 1.0));
                async {}
            },
            (),
        )
        .await
        .map_err(|e| AppError::DeviceComs(e.to_string()))?;

    Ok(())
}

pub async fn sideload_with_progress(
    op: &Operation<'_>,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
) -> Result<Option<SpecialApp>, AppError> {
    op.start("prepare")?;
    let app_path_buf: PathBuf = app_path.into();
    let original_bytes = op.fail_if_err("prepare", calculate_total_size(&app_path_buf))?;
    op.progress_bytes("prepare", original_bytes, original_bytes.max(1))?;
    op.complete("prepare")?;

    let device = {
        let device_lock = device_state.lock().unwrap();
        match &*device_lock {
            Some(d) => d.clone(),
            None => return Err(AppError::NoDeviceSelected),
        }
    };

    let provider = get_provider(&device.info).await?;

    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    op.start("sign")?;
    op.progress("sign", 0.1)?;

    let device_info =
        op.fail_if_err("sign", IdeviceInfo::from_device(&provider).await.map_err(AppError::from))?;

    let team = op.fail_if_err(
        "sign",
        sideloader.get_mut().get_team().await.map_err(AppError::from),
    )?;
    op.progress("sign", 0.3)?;

    op.fail_if_err(
        "sign",
        sideloader
            .get_mut()
            .get_dev_session()
            .ensure_device_registered(&team, &device_info.name, &device_info.udid, None)
            .await
            .map_err(AppError::from),
    )?;

    // For large IPAs, signing can take a while. Keep progress moving predictably
    // with conservative throughput assumptions so it doesn't stall at low percentages.
    let cpu_threads = std::thread::available_parallelism()
        .map(|n| n.get() as f64)
        .unwrap_or(8.0);
    let estimated_sign_throughput_mb_s = (18.0 + cpu_threads * 1.8).clamp(20.0, 95.0);
    let estimated_sign_secs = ((original_bytes as f64
        / (estimated_sign_throughput_mb_s * 1024.0 * 1024.0))
        .ceil() as u64)
        .clamp(8, 240);

    let sign_start = Instant::now();
    let mut ticker = interval(Duration::from_millis(180));
    op.progress("sign", 0.2)?;

    let mut sign_future = Box::pin(sideloader.get_mut().sign_app(app_path_buf, Some(team), false));

    let (signed_app_path, special) = loop {
        tokio::select! {
            result = &mut sign_future => {
                let signed = op.fail_if_err("sign", result.map_err(AppError::from))?;
                break signed;
            }
            _ = ticker.tick() => {
                let elapsed = sign_start.elapsed().as_secs_f64();
                let linear = (elapsed / estimated_sign_secs as f64).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - linear).powf(1.15);

                // Sign stage should visually progress across most of its range.
                let mut visual = 0.2 + eased * 0.76;

                // If estimate is exceeded, continue creeping to avoid apparent freeze.
                if elapsed > estimated_sign_secs as f64 {
                    let overtime = (elapsed - estimated_sign_secs as f64).min(180.0);
                    visual = visual.max(0.90 + (overtime / 180.0) * 0.08);
                }

                let _ = op.progress("sign", visual.min(0.98));
            }
        }
    };

    op.progress("sign", 1.0)?;
    op.complete("sign")?;

    op.start("transfer")?;
    op.progress("transfer", 0.01)?;

    op.fail_if_err(
        "transfer",
        upload_signed_bundle(&provider, &signed_app_path, |uploaded, total| {
            let _ = op.progress_bytes("transfer", uploaded, total);
        })
        .await,
    )?;

    op.progress("transfer", 1.0)?;
    op.complete("transfer")?;

    op.start("install")?;
    op.progress("install", 0.01)?;

    op.fail_if_err(
        "install",
        install_signed_bundle(&provider, &signed_app_path, |ratio| {
            let _ = op.progress("install", ratio);
        })
        .await,
    )?;

    op.progress("install", 1.0)?;
    op.complete("install")?;

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
    sideload_with_progress(&op, device_state, sideloader_state, app_path).await?;
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
    op.fail_if_err(
        "download",
        download(url, &dest, |downloaded, total| {
            let _ = op.progress_bytes("download", downloaded, total);
        })
        .await,
    )?;
    op.move_on("download", "install")?;
    let device = {
        let device_guard = device_state.lock().unwrap();
        match &*device_guard {
            Some(d) => d.clone(),
            None => return op.fail("install", AppError::NoDeviceSelected),
        }
    };
    let _special = op.fail_if_err(
        "install",
        sideload_with_progress(
            &op,
            device_state,
            sideloader_state,
            dest.to_string_lossy().to_string(),
        )
        .await,
    )?;

    op.start("pairing")?;
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
        op.complete("pairing")?;
    } else {
        op.complete("pairing")?;
    }
    Ok(())
}

pub async fn download(
    url: impl AsRef<str>,
    dest: &PathBuf,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), AppError> {
    let response = reqwest::get(url.as_ref())
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    if !response.status().is_success() {
        return Err(AppError::Download(format!(
            "Failed to download file: HTTP {}",
            response.status()
        )));
    }

    let total = response.content_length().unwrap_or(1).max(1);
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(dest).await.map_err(|e| {
        AppError::Filesystem("Failed to create downloaded file".into(), e.to_string())
    })?;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::Download(e.to_string()))?;
        file.write_all(&chunk).await.map_err(|e| {
            AppError::Filesystem("Failed to write downloaded file".into(), e.to_string())
        })?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        on_progress(downloaded.min(total), total);
    }

    file.flush().await.map_err(|e| {
        AppError::Filesystem("Failed to flush downloaded file".into(), e.to_string())
    })?;

    if downloaded == 0 {
        on_progress(1, 1);
    }

    Ok(())
}
