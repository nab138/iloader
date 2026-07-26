use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::{
    device::{DeviceInfoMutex, get_provider, get_provider_from_connection, get_usbmuxd},
    error::AppError,
    operation::Operation,
    pairing::{get_sidestore_info, place_file},
    secure_storage::create_sideloading_storage,
};
use apple_codesign::{SettingsScope, UnifiedSigner};
use idevice::provider::IdeviceProvider;
use isideload::{
    dev::{
        app_groups::AppGroupsApi,
        app_ids::{AppId, AppIdsApi},
        devices::DevicesApi,
        teams::DeveloperTeam,
    },
    sideload::{
        application::{Application, SpecialApp},
        builder::MaxCertsBehavior,
        cert_identity::CertificateIdentity,
        sideloader::Sideloader,
        sign::signing_settings,
    },
    util::device::IdeviceInfo,
};
use plist::Dictionary;
use plist_macro::plist_to_xml_string;
use rootcause::{Report, option_ext::OptionExt, prelude::*};
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
    sign_app_extensions: bool,
) -> Result<Option<SpecialApp>, AppError> {
    let device = {
        let device_lock = device_state.lock().unwrap();
        match &*device_lock {
            Some(d) => d.clone(),
            None => return Err(AppError::NoDeviceSelected),
        }
    };

    let provider = get_provider(&device.info).await?;

    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    let special = if sign_app_extensions {
        install_app_with_extensions(handle, &provider, sideloader.get_mut(), app_path.into())
            .await?
    } else {
        sideloader
            .get_mut()
            .install_app(&provider, app_path.into(), false)
            .await?
    };

    Ok(special)
}

#[tauri::command]
pub async fn sideload_operation(
    handle: AppHandle,
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    app_path: String,
    sign_app_extensions: bool,
) -> Result<(), AppError> {
    let op = Operation::new("sideload".to_string(), &window);
    op.start("install")?;
    op.fail_if_err(
        "install",
        sideload(
            &handle,
            device_state,
            sideloader_state,
            app_path,
            sign_app_extensions,
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
            false,
        )
        .await,
    )?;
    op.move_on("install", "pairing")?;
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

async fn install_app_with_extensions(
    handle: &AppHandle,
    device_provider: &impl IdeviceProvider,
    sideloader: &mut Sideloader,
    app_path: PathBuf,
) -> Result<Option<SpecialApp>, AppError> {
    let device_info = IdeviceInfo::from_device(device_provider).await?;
    let team = sideloader.get_team().await?;
    sideloader
        .get_dev_session()
        .ensure_device_registered(&team, &device_info.name, &device_info.udid, None)
        .await?;

    let (signed_app_path, special_app) = sideloader
        .sign_app(app_path, Some(team.clone()), false)
        .await?;

    let result: Result<(), AppError> = async {
        resign_app_extensions(handle, sideloader, &signed_app_path, &team, &special_app).await?;

        info!("Transferring App...");
        isideload::sideload::install::install_app(device_provider, &signed_app_path, |progress| {
            info!("Installing: {}%", progress);
        })
        .await?;

        Ok(())
    }
    .await;

    if let Err(error) = tokio::fs::remove_dir_all(&signed_app_path).await {
        warn!(
            "Failed to remove temporary signed app file {}: {}",
            signed_app_path.display(),
            error
        );
    }

    result?;
    Ok(special_app)
}

async fn resign_app_extensions(
    handle: &AppHandle,
    sideloader: &mut Sideloader,
    signed_app_path: &Path,
    team: &DeveloperTeam,
    special_app: &Option<SpecialApp>,
) -> Result<(), AppError> {
    let app = Application::new(signed_app_path.to_path_buf())?;
    let main_bundle_id = app.main_bundle_id()?;
    let main_app_name = app.main_app_name()?;
    let main_bundle_path = app.bundle.bundle_dir.clone();

    let mut extensions = Vec::new();
    for bundle in app.bundle.collect_nested_bundles() {
        if bundle
            .bundle_dir
            .extension()
            .is_none_or(|ext| ext != "appex")
        {
            continue;
        }

        let bundle_id = bundle
            .bundle_identifier()
            .ok_or_else(|| {
                AppError::Misc(format!(
                    "App extension {} has no bundle identifier",
                    bundle.bundle_dir.display()
                ))
            })?
            .to_string();
        extensions.push((bundle.bundle_dir, bundle_id));
    }

    if extensions.is_empty() {
        info!("No app extensions found");
        return Ok(());
    }

    extensions.sort_by_key(|(path, _)| path.components().count());
    extensions.reverse();
    let requested_app_groups = vec![app_group_for_bundle_id(&main_bundle_id)];

    let mut target_bundle_ids = BTreeSet::from([main_bundle_id.clone()]);
    target_bundle_ids.extend(extensions.iter().map(|(_, bundle_id)| bundle_id.clone()));

    let mut app_ids: Vec<AppId> = sideloader
        .get_dev_session()
        .list_app_ids(team, None)
        .await?
        .app_ids
        .into_iter()
        .filter(|app_id| target_bundle_ids.contains(&app_id.identifier))
        .collect();

    configure_requested_app_groups(
        sideloader,
        team,
        &main_app_name,
        &mut app_ids,
        &requested_app_groups,
    )
    .await?;

    let main_app_id = app_ids
        .iter()
        .find(|app_id| app_id.identifier == main_bundle_id)
        .cloned()
        .ok_or_else(|| {
            AppError::Misc(format!(
                "Main app ID {} not found in registered app IDs",
                main_bundle_id
            ))
        })?;

    let apple_email = sideloader.get_email().to_string();
    let storage = create_sideloading_storage(handle)?;
    let cert_identity = CertificateIdentity::retrieve(
        "iloader",
        &apple_email,
        sideloader.get_dev_session(),
        team,
        storage.as_ref(),
        &MaxCertsBehavior::Error,
    )
    .await?;

    for (extension_path, bundle_id) in extensions {
        let app_id = app_ids
            .iter()
            .find(|app_id| app_id.identifier == bundle_id)
            .cloned()
            .ok_or_else(|| {
                AppError::Misc(format!(
                    "App extension ID {} not found in registered app IDs",
                    bundle_id
                ))
            })?;
        let profile = sideloader
            .get_dev_session()
            .download_team_provisioning_profile(team, &app_id, None)
            .await?;

        tokio::fs::write(
            extension_path.join("embedded.mobileprovision"),
            profile.encoded_profile.as_ref(),
        )
        .await
        .map_err(|error| {
            AppError::Filesystem(
                format!(
                    "Failed to write provisioning profile for app extension {}",
                    extension_path.display()
                ),
                error.to_string(),
            )
        })?;

        sign_bundle(
            &extension_path,
            &cert_identity,
            profile.encoded_profile.as_ref(),
            &None,
            team,
            &requested_app_groups,
        )?;
        info!("Signed app extension {}", bundle_id);
    }

    let main_profile = sideloader
        .get_dev_session()
        .download_team_provisioning_profile(team, &main_app_id, None)
        .await?;
    sign_bundle(
        &main_bundle_path,
        &cert_identity,
        main_profile.encoded_profile.as_ref(),
        special_app,
        team,
        &requested_app_groups,
    )?;

    Ok(())
}

async fn configure_requested_app_groups(
    sideloader: &mut Sideloader,
    team: &DeveloperTeam,
    main_app_name: &str,
    app_ids: &mut [AppId],
    requested_app_groups: &[String],
) -> Result<(), Report> {
    for group_identifier in requested_app_groups {
        let app_group = sideloader
            .get_dev_session()
            .ensure_app_group(team, main_app_name, group_identifier, None)
            .await
            .context(format!(
                "Failed to register requested app group {group_identifier}"
            ))?;

        for app_id in app_ids.iter_mut() {
            app_id
                .ensure_group_feature(sideloader.get_dev_session(), team)
                .await
                .context(format!(
                    "Failed to enable app groups for {}",
                    app_id.identifier
                ))?;
            sideloader
                .get_dev_session()
                .assign_app_group(team, &app_group, app_id, None)
                .await
                .context(format!(
                    "Failed to assign app group {group_identifier} to {}",
                    app_id.identifier
                ))?;
        }

        info!(
            "Configured requested app group {} for {} app IDs",
            group_identifier,
            app_ids.len()
        );
    }

    Ok(())
}

fn sign_bundle(
    bundle_path: &Path,
    cert_identity: &CertificateIdentity,
    provisioning_profile: &[u8],
    special_app: &Option<SpecialApp>,
    team: &DeveloperTeam,
    requested_app_groups: &[String],
) -> Result<(), Report> {
    let mut settings = signing_settings(cert_identity)?;
    let entitlements = entitlements_from_profile(provisioning_profile, special_app, team)?;
    validate_requested_app_groups(&entitlements, requested_app_groups, bundle_path)?;
    settings
        .set_entitlements_xml(SettingsScope::Main, plist_to_xml_string(&entitlements))
        .context("Failed to set entitlements XML")?;

    UnifiedSigner::new(settings)
        .sign_path_in_place(bundle_path)
        .context(format!("Failed to sign bundle: {}", bundle_path.display()))?;

    Ok(())
}

fn entitlements_from_profile(
    data: &[u8],
    special_app: &Option<SpecialApp>,
    team: &DeveloperTeam,
) -> Result<Dictionary, Report> {
    let start = data
        .windows(6)
        .position(|window| window == b"<plist")
        .ok_or_report()?;
    let end = data
        .windows(8)
        .rposition(|window| window == b"</plist>")
        .ok_or_report()?
        + 8;
    let plist = plist::Value::from_reader_xml(&data[start..end])?;
    let mut entitlements = plist
        .as_dictionary()
        .ok_or_report()?
        .get("Entitlements")
        .and_then(plist::Value::as_dictionary)
        .ok_or_report()?
        .clone();

    if matches!(
        special_app,
        Some(SpecialApp::SideStoreLc) | Some(SpecialApp::LiveContainer)
    ) {
        let mut keychain_access = vec![plist::Value::String(format!(
            "{}.com.kdt.livecontainer.shared",
            team.team_id
        ))];

        for number in 1..128 {
            keychain_access.push(plist::Value::String(format!(
                "{}.com.kdt.livecontainer.shared.{}",
                team.team_id, number
            )));
        }

        entitlements.insert(
            "keychain-access-groups".to_string(),
            plist::Value::Array(keychain_access),
        );
    }

    Ok(entitlements)
}

fn app_group_for_bundle_id(bundle_id: &str) -> String {
    format!("group.{bundle_id}")
}

fn validate_requested_app_groups(
    entitlements: &Dictionary,
    requested_app_groups: &[String],
    bundle_path: &Path,
) -> Result<(), Report> {
    if requested_app_groups.is_empty() {
        return Ok(());
    }

    let profile_app_groups = entitlements
        .get("com.apple.security.application-groups")
        .and_then(plist::Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(plist::Value::as_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let missing_groups = requested_app_groups
        .iter()
        .filter(|group| !profile_app_groups.contains(group.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    if !missing_groups.is_empty() {
        bail!(
            "Provisioning profile for {} is missing requested app groups: {}",
            bundle_path.display(),
            missing_groups.join(", ")
        );
    }

    info!(
        "Provisioning profile for {} contains requested app groups: {}",
        bundle_path.display(),
        requested_app_groups.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{app_group_for_bundle_id, validate_requested_app_groups};
    use plist::{Dictionary, Value};
    use std::path::Path;

    #[test]
    fn derives_app_group_from_bundle_id() {
        assert_eq!(
            app_group_for_bundle_id("com.example.app.A1B2C3D4E5"),
            "group.com.example.app.A1B2C3D4E5"
        );
    }

    #[test]
    fn derives_app_group_for_other_namespaces() {
        assert_eq!(
            app_group_for_bundle_id("org.example.product.Z9Y8X7W6V5"),
            "group.org.example.product.Z9Y8X7W6V5"
        );
    }

    #[test]
    fn accepts_profile_with_requested_app_group() {
        let mut entitlements = Dictionary::new();
        entitlements.insert(
            "com.apple.security.application-groups".to_string(),
            Value::Array(vec![Value::String(
                "group.com.example.app.A1B2C3D4E5".to_string(),
            )]),
        );

        validate_requested_app_groups(
            &entitlements,
            &["group.com.example.app.A1B2C3D4E5".to_string()],
            Path::new("Runner.app"),
        )
        .unwrap();
    }

    #[test]
    fn rejects_profile_without_requested_app_group() {
        let error = validate_requested_app_groups(
            &Dictionary::new(),
            &["group.com.example.app.A1B2C3D4E5".to_string()],
            Path::new("ExampleWidget.appex"),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("group.com.example.app.A1B2C3D4E5")
        );
    }
}
