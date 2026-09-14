use idevice::{
    IdeviceService, RsdService,
    afc::AfcClient,
    installation_proxy::InstallationProxyClient,
    provider::{IdeviceProvider, RsdProvider},
    rsd::RsdHandshake,
};
use plist_macro::plist;
use rootcause::option_ext::OptionExt;
use rootcause::prelude::*;

use crate::SideloadError as Error;
use std::pin::Pin;
use std::{future::Future, path::Path};

/// Installs an ***already signed*** app onto your device.
/// To sign and install an app, see [`crate::sideload::sideload_app`]
pub async fn install_app(
    provider: &impl IdeviceProvider,
    app_path: &Path,
    progress_callback: impl Fn(u64) + Send + Sync,
) -> Result<(), Report> {
    let mut afc_client = AfcClient::connect(provider)
        .await
        .map_err(Error::IdeviceError)?;

    let dir = format!(
        "PublicStaging/{}",
        app_path.file_name().ok_or_report()?.to_string_lossy()
    );
    let total_size = get_dir_size(app_path).unwrap_or(1) as f64;
    let mut uploaded = 0;
    let cb = |progress: f64| {
        progress_callback((progress * 70.0) as u64);
    };
    afc_upload_dir(
        &mut afc_client,
        app_path,
        &dir,
        &cb,
        &mut uploaded,
        total_size,
    )
    .await?;

    let mut instproxy_client = InstallationProxyClient::connect(provider)
        .await
        .map_err(Error::IdeviceError)?;

    let options = plist!(dict {
        "PackageType": "Developer"
    });

    instproxy_client
        .install_with_callback(
            dir,
            Some(plist::Value::Dictionary(options)),
            async |(percentage, _)| {
                progress_callback((70.0 + 0.3 * percentage as f64) as u64);
            },
            (),
        )
        .await
        .map_err(Error::IdeviceError)?;

    Ok(())
}

/// Installs an ***already signed*** app onto your device.
/// To sign and install an app, see [`crate::sideload::sideload_app`]
pub async fn install_app_rsd(
    provider: &mut impl RsdProvider,
    handshake: &mut RsdHandshake,
    app_path: &Path,
    progress_callback: impl Fn(u64) + Send + Sync,
) -> Result<(), Report> {
    let mut afc_client = AfcClient::connect_rsd(provider, handshake)
        .await
        .map_err(Error::IdeviceError)?;

    let dir = format!(
        "PublicStaging/{}",
        app_path.file_name().ok_or_report()?.to_string_lossy()
    );
    let total_size = get_dir_size(app_path).unwrap_or(1) as f64;
    let mut uploaded = 0;
    let cb = |pct: f64| {
        progress_callback((pct * 70.0) as u64);
    };
    afc_upload_dir(
        &mut afc_client,
        app_path,
        &dir,
        &cb,
        &mut uploaded,
        total_size,
    )
    .await?;

    let mut instproxy_client = InstallationProxyClient::connect_rsd(provider, handshake)
        .await
        .map_err(Error::IdeviceError)?;

    let options = plist!(dict {
        "PackageType": "Developer"
    });

    instproxy_client
        .install_with_callback(
            dir,
            Some(plist::Value::Dictionary(options)),
            async |(percentage, _)| {
                progress_callback((70.0 + 0.3 * percentage as f64) as u64);
            },
            (),
        )
        .await
        .map_err(Error::IdeviceError)?;

    Ok(())
}

fn get_dir_size(path: &Path) -> Result<u64, Report> {
    let mut size = 0;
    for entry in isideload_vfs::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        let meta = isideload_vfs::fs::metadata(&path)?;
        if meta.is_dir() {
            size += get_dir_size(&path)?;
        } else {
            size += meta.len();
        }
    }
    Ok(size)
}

fn afc_upload_dir<'a>(
    afc_client: &'a mut AfcClient,
    path: &'a Path,
    afc_path: &'a str,
    cb: &'a (dyn Fn(f64) + Send + Sync),
    uploaded: &'a mut u64,
    total: f64,
) -> Pin<Box<dyn Future<Output = Result<(), Report>> + Send + 'a>> {
    Box::pin(async move {
        let entries = isideload_vfs::fs::read_dir(path)?;
        afc_client
            .mk_dir(afc_path)
            .await
            .map_err(Error::IdeviceError)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if isideload_vfs::fs::metadata(&path)?.is_dir() {
                let new_afc_path = format!(
                    "{}/{}",
                    afc_path,
                    path.file_name().ok_or_report()?.to_string_lossy()
                );
                afc_upload_dir(afc_client, &path, &new_afc_path, cb, uploaded, total).await?;
            } else {
                let mut file_handle = afc_client
                    .open(
                        format!(
                            "{}/{}",
                            afc_path,
                            path.file_name().ok_or_report()?.to_string_lossy()
                        ),
                        idevice::afc::opcode::AfcFopenMode::WrOnly,
                    )
                    .await
                    .map_err(Error::IdeviceError)?;

                let bytes = isideload_vfs::fs::read(&path)?;
                for chunk in bytes.chunks(8 * 1024) {
                    file_handle
                        .write_entire(chunk)
                        .await
                        .map_err(Error::IdeviceError)?;
                    *uploaded += chunk.len() as u64;
                    cb(*uploaded as f64 / total);
                }
                file_handle.close().await.map_err(Error::IdeviceError)?;
            }
        }
        Ok(())
    })
}
