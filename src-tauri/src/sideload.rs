use std::{
    collections::{BTreeMap, BTreeSet},
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
use apple_codesign::{
    BundleSigningContext, CodeResourcesBuilder, CodeResourcesRule, MachFile, SettingsScope,
    SignedMachOInfo, UnifiedSigner,
};
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
        bundle::Bundle,
        cert_identity::CertificateIdentity,
        sideloader::Sideloader,
        sign::signing_settings,
    },
    util::device::IdeviceInfo,
};
use plist::Dictionary;
use plist_macro::plist_to_xml_string;
use rootcause::{Report, option_ext::OptionExt, prelude::*};
use sha2::{Digest, Sha256};
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

    let app = Application::new(app_path)?;
    let original_app_groups = collect_original_app_groups(&app);
    let app_group_mappings = remap_app_groups(&original_app_groups, &team.team_id)?;
    patch_app_group_references(&app.bundle.bundle_dir, &app_group_mappings)?;
    let alt_app_groups = collect_alt_app_groups(&app.bundle.bundle_dir)?;

    let (signed_app_path, special_app) = sideloader
        .sign_app(app.bundle.bundle_dir.clone(), Some(team.clone()), false)
        .await?;
    let signed_main_bundle_id = Application::new(signed_app_path.clone())?.main_bundle_id()?;
    let requested_app_groups = required_app_groups(
        &app_group_mappings,
        &signed_main_bundle_id,
        &special_app,
        &team.team_id,
    );

    let result: Result<(), AppError> = async {
        restore_alt_app_groups(&signed_app_path, &alt_app_groups)?;
        resign_app_extensions(
            handle,
            sideloader,
            &signed_app_path,
            &team,
            &special_app,
            &requested_app_groups,
        )
        .await?;

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
    requested_app_groups: &[String],
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
    let extension_bundle_paths = extensions
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();

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
        requested_app_groups,
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

        write_provisioning_profile(&extension_path, profile.encoded_profile.as_ref()).await?;

        sign_bundle(
            &extension_path,
            &bundle_id,
            &[],
            &cert_identity,
            profile.encoded_profile.as_ref(),
            &None,
            team,
            requested_app_groups,
        )?;
        info!("Signed app extension {}", bundle_id);
    }

    let main_profile = sideloader
        .get_dev_session()
        .download_team_provisioning_profile(team, &main_app_id, None)
        .await?;
    write_provisioning_profile(&main_bundle_path, main_profile.encoded_profile.as_ref()).await?;
    sign_bundle(
        &main_bundle_path,
        &main_bundle_id,
        &extension_bundle_paths,
        &cert_identity,
        main_profile.encoded_profile.as_ref(),
        special_app,
        team,
        requested_app_groups,
    )?;

    Ok(())
}

async fn write_provisioning_profile(
    bundle_path: &Path,
    provisioning_profile: &[u8],
) -> Result<(), AppError> {
    tokio::fs::write(
        bundle_path.join("embedded.mobileprovision"),
        provisioning_profile,
    )
    .await
    .map_err(|error| {
        AppError::Filesystem(
            format!(
                "Failed to write provisioning profile for {}",
                bundle_path.display()
            ),
            error.to_string(),
        )
    })
}

fn validate_nested_app_extension_seals(
    main_bundle_path: &Path,
    extension_bundle_paths: &[PathBuf],
) -> Result<(), Report> {
    if extension_bundle_paths.is_empty() {
        return Ok(());
    }

    let code_resources_path = main_bundle_path
        .join("_CodeSignature")
        .join("CodeResources");
    let code_resources_data = std::fs::read(&code_resources_path).context(format!(
        "Failed to read nested code seals from {}",
        code_resources_path.display()
    ))?;
    let code_resources =
        plist::Value::from_reader_xml(code_resources_data.as_slice()).context(format!(
            "Failed to parse nested code seals from {}",
            code_resources_path.display()
        ))?;
    let files2 = code_resources
        .as_dictionary()
        .and_then(|dictionary| dictionary.get("files2"))
        .and_then(plist::Value::as_dictionary)
        .ok_or_report()
        .context(format!(
            "Main app signature {} has no files2 code seals",
            code_resources_path.display()
        ))?;

    for extension_path in extension_bundle_paths {
        let relative_path = extension_path
            .strip_prefix(main_bundle_path)
            .context(format!(
                "Failed to resolve app extension {} relative to {}",
                extension_path.display(),
                main_bundle_path.display()
            ))?
            .to_string_lossy()
            .replace('\\', "/");
        let actual_code_seal = nested_code_seal(files2, &relative_path)
            .ok_or_report()
            .context(format!(
                "Main app signature is missing the nested code seal for {}",
                relative_path
            ))?;
        let extension_bundle = Bundle::new(extension_path.clone())?;
        let executable_name = extension_bundle
            .app_info
            .get("CFBundleExecutable")
            .and_then(plist::Value::as_string)
            .ok_or_report()
            .context(format!(
                "App extension {} has no executable",
                extension_path.display()
            ))?;
        let executable_path = extension_path.join(executable_name);
        let executable_data = std::fs::read(&executable_path).context(format!(
            "Failed to read signed app extension executable {}",
            executable_path.display()
        ))?;
        let signed_info = SignedMachOInfo::parse_data(&executable_data)?;
        let expected_code_seal = Sha256::digest(&signed_info.code_directory_blob);
        if actual_code_seal != &expected_code_seal[..20] {
            bail!(
                "Main app signature has an invalid nested code seal for {}",
                relative_path
            );
        }

        info!("Verified nested app extension code seal: {}", relative_path);
    }

    Ok(())
}

fn nested_code_seal<'a>(files2: &'a Dictionary, relative_path: &str) -> Option<&'a [u8]> {
    files2
        .get(relative_path)
        .and_then(plist::Value::as_dictionary)
        .and_then(|seal| seal.get("cdhash"))
        .and_then(plist::Value::as_data)
}

fn apply_nested_app_extension_seals(
    main_bundle_path: &Path,
    extension_bundle_paths: &[PathBuf],
    cert_identity: &CertificateIdentity,
    entitlements: &Dictionary,
) -> Result<(), Report> {
    let main_bundle = Bundle::new(main_bundle_path.to_path_buf())?;
    let executable_name = main_bundle
        .app_info
        .get("CFBundleExecutable")
        .and_then(plist::Value::as_string)
        .ok_or_report()
        .context(format!(
            "Main app bundle {} has no executable",
            main_bundle_path.display()
        ))?;

    let mut resources = CodeResourcesBuilder::default_no_resources_rules()?;
    resources.add_rule2(
        CodeResourcesRule::new("^(PlugIns|Plug-ins)/")?
            .nested()
            .weight(10),
    );
    resources.add_exclusion_rule(CodeResourcesRule::new("^_CodeSignature/")?.exclude());
    resources.add_exclusion_rule(CodeResourcesRule::new("^CodeResources$")?.exclude());
    resources.add_exclusion_rule(CodeResourcesRule::new("^_MASReceipt$")?.exclude());
    resources.add_exclusion_rule(
        CodeResourcesRule::new(format!("^{}$", regex::escape(executable_name)))?.exclude(),
    );

    let mut settings = signing_settings(cert_identity)?;
    settings
        .set_entitlements_xml(SettingsScope::Main, plist_to_xml_string(entitlements))
        .context("Failed to set main app entitlements XML")?;
    {
        let mut context = BundleSigningContext {
            settings: &settings,
            dest_dir: main_bundle_path.to_path_buf(),
            previously_installed_paths: BTreeSet::new(),
            installed_paths: BTreeSet::new(),
        };
        resources
            .walk_and_seal_directory(main_bundle_path, main_bundle_path, &mut context)
            .context(format!(
                "Failed to seal nested app extensions in {}",
                main_bundle_path.display()
            ))?;
    }

    let mut resources_data = Vec::new();
    resources
        .write_code_resources(&mut resources_data)
        .context("Failed to encode main app CodeResources")?;
    let code_resources_path = main_bundle_path
        .join("_CodeSignature")
        .join("CodeResources");
    std::fs::write(&code_resources_path, &resources_data).context(format!(
        "Failed to write nested code seals to {}",
        code_resources_path.display()
    ))?;

    settings.set_code_resources_data(SettingsScope::Main, resources_data);
    UnifiedSigner::new(settings)
        .sign_path_in_place(main_bundle_path.join(executable_name))
        .context(format!(
            "Failed to sign main executable with {} nested app extension seal(s)",
            extension_bundle_paths.len()
        ))?;
    info!(
        "Applied {} nested app extension code seal(s)",
        extension_bundle_paths.len()
    );
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
    expected_bundle_id: &str,
    nested_extension_paths: &[PathBuf],
    cert_identity: &CertificateIdentity,
    provisioning_profile: &[u8],
    special_app: &Option<SpecialApp>,
    team: &DeveloperTeam,
    requested_app_groups: &[String],
) -> Result<(), Report> {
    let mut settings = signing_settings(cert_identity)?;
    let entitlements = entitlements_from_profile(provisioning_profile, special_app, team)?;
    validate_profile_identity(&entitlements, expected_bundle_id, team, bundle_path)?;
    validate_requested_app_groups(&entitlements, requested_app_groups, bundle_path)?;
    settings
        .set_entitlements_xml(SettingsScope::Main, plist_to_xml_string(&entitlements))
        .context("Failed to set entitlements XML")?;

    UnifiedSigner::new(settings)
        .sign_path_in_place(bundle_path)
        .context(format!("Failed to sign bundle: {}", bundle_path.display()))?;

    if !nested_extension_paths.is_empty() {
        apply_nested_app_extension_seals(
            bundle_path,
            nested_extension_paths,
            cert_identity,
            &entitlements,
        )?;
        validate_nested_app_extension_seals(bundle_path, nested_extension_paths)?;
    }

    validate_signed_bundle(bundle_path, expected_bundle_id, team, requested_app_groups)?;

    Ok(())
}

fn entitlements_from_profile(
    data: &[u8],
    special_app: &Option<SpecialApp>,
    team: &DeveloperTeam,
) -> Result<Dictionary, Report> {
    let mut entitlements = provisioning_profile_entitlements(data)?;

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

fn provisioning_profile_entitlements(data: &[u8]) -> Result<Dictionary, Report> {
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
    let entitlements = plist
        .as_dictionary()
        .ok_or_report()?
        .get("Entitlements")
        .and_then(plist::Value::as_dictionary)
        .ok_or_report()?
        .clone();

    Ok(entitlements)
}

fn collect_alt_app_groups(bundle_path: &Path) -> Result<BTreeMap<PathBuf, plist::Value>, Report> {
    let app = Application::new(bundle_path.to_path_buf())?;
    let mut bundles = app.bundle.collect_nested_bundles();
    bundles.push(app.bundle);

    let mut alt_app_groups = BTreeMap::new();
    for bundle in bundles {
        let Some(value) = bundle.app_info.get("ALTAppGroups") else {
            continue;
        };
        let relative_path = bundle
            .bundle_dir
            .strip_prefix(bundle_path)
            .context(format!(
                "Failed to resolve bundle path {} relative to {}",
                bundle.bundle_dir.display(),
                bundle_path.display()
            ))?
            .to_path_buf();
        alt_app_groups.insert(relative_path, value.clone());
    }

    Ok(alt_app_groups)
}

fn restore_alt_app_groups(
    bundle_path: &Path,
    alt_app_groups: &BTreeMap<PathBuf, plist::Value>,
) -> Result<(), Report> {
    for (relative_path, value) in alt_app_groups {
        let info_path = bundle_path.join(relative_path).join("Info.plist");
        let data =
            std::fs::read(&info_path).context(format!("Failed to read {}", info_path.display()))?;
        let mut info: Dictionary =
            plist::from_bytes(&data).context(format!("Failed to parse {}", info_path.display()))?;
        info.insert("ALTAppGroups".to_string(), value.clone());
        plist::to_file_binary(&info_path, &info)
            .context(format!("Failed to write {}", info_path.display()))?;
        info!(
            "Restored ALTAppGroups for {}",
            bundle_path.join(relative_path).display()
        );
    }

    Ok(())
}

fn collect_original_app_groups(app: &Application) -> Vec<String> {
    let mut app_groups = BTreeSet::new();
    let mut bundles = app.bundle.collect_nested_bundles();
    bundles.push(app.bundle.clone());

    for bundle in bundles {
        let is_app_or_extension = bundle
            .bundle_dir
            .extension()
            .is_some_and(|extension| extension == "app" || extension == "appex");
        if !is_app_or_extension {
            continue;
        }

        match original_entitlements(&bundle) {
            Ok(Some(entitlements)) => {
                app_groups.extend(app_groups_from_entitlements(&entitlements));
            }
            Ok(None) => {}
            Err(error) => warn!(
                "Failed to read original entitlements from {}: {}",
                bundle.bundle_dir.display(),
                error
            ),
        }
    }

    let app_groups = app_groups.into_iter().collect::<Vec<_>>();
    if app_groups.is_empty() {
        info!("No original app groups found");
    } else {
        info!("Found original app groups: {}", app_groups.join(", "));
    }

    app_groups
}

fn code_signature_entitlements(bundle: &Bundle) -> Result<Option<Dictionary>, Report> {
    if let Some(executable_name) = bundle
        .app_info
        .get("CFBundleExecutable")
        .and_then(plist::Value::as_string)
    {
        let executable_data = std::fs::read(bundle.bundle_dir.join(executable_name))?;
        let mach_file = MachFile::parse(&executable_data)?;

        for macho in mach_file.iter_macho() {
            if let Some(signature) = macho.code_signature()?
                && let Some(entitlements) = signature.entitlements()?
            {
                let plist = plist::Value::from_reader_xml(entitlements.as_str().as_bytes())?;
                if let Some(entitlements) = plist.into_dictionary() {
                    return Ok(Some(entitlements));
                }
            }
        }
    }

    Ok(None)
}

fn original_entitlements(bundle: &Bundle) -> Result<Option<Dictionary>, Report> {
    if let Some(entitlements) = code_signature_entitlements(bundle)? {
        return Ok(Some(entitlements));
    }

    let provisioning_profile_path = bundle.bundle_dir.join("embedded.mobileprovision");
    if provisioning_profile_path.exists() {
        let provisioning_profile = std::fs::read(provisioning_profile_path)?;
        return Ok(Some(provisioning_profile_entitlements(
            &provisioning_profile,
        )?));
    }

    Ok(None)
}

fn app_groups_from_entitlements(entitlements: &Dictionary) -> Vec<String> {
    entitlements
        .get("com.apple.security.application-groups")
        .and_then(plist::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(plist::Value::as_string)
        .map(str::to_string)
        .collect()
}

fn required_app_groups(
    app_group_mappings: &[(String, String)],
    signed_main_bundle_id: &str,
    special_app: &Option<SpecialApp>,
    team_id: &str,
) -> Vec<String> {
    let mut app_groups = app_group_mappings
        .iter()
        .map(|(_, replacement)| replacement.clone())
        .collect::<BTreeSet<_>>();
    let default_app_group = if matches!(special_app, Some(SpecialApp::SideStoreLc)) {
        format!("group.com.SideStore.SideStore.{team_id}")
    } else {
        format!("group.{signed_main_bundle_id}")
    };
    app_groups.insert(default_app_group);
    app_groups.into_iter().collect()
}

fn remap_app_groups(
    original_app_groups: &[String],
    team_id: &str,
) -> Result<Vec<(String, String)>, Report> {
    let mut replacements = BTreeSet::new();
    let mut mappings = Vec::with_capacity(original_app_groups.len());

    for original in original_app_groups {
        if !original.starts_with("group.") || !original.is_ascii() {
            bail!("Unsupported app group identifier: {original}");
        }

        let suffix_length = original.len() - "group.".len();
        if suffix_length == 0 {
            bail!("Unsupported empty app group identifier: {original}");
        }

        let hash = app_group_hash(team_id, original);
        let hash = format!("{hash:016x}");
        let replacement_suffix = hash.chars().cycle().take(suffix_length).collect::<String>();
        let replacement = format!("group.{replacement_suffix}");

        if !replacements.insert(replacement.clone()) {
            bail!("App group remapping collision for {original}");
        }

        info!("Remapping app group {} to {}", original, replacement);
        mappings.push((original.clone(), replacement));
    }

    mappings.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
    Ok(mappings)
}

fn app_group_hash(team_id: &str, app_group: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    team_id
        .bytes()
        .chain([0])
        .chain(app_group.bytes())
        .fold(FNV_OFFSET_BASIS, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        })
}

fn patch_app_group_references(
    bundle_path: &Path,
    mappings: &[(String, String)],
) -> Result<(), Report> {
    if mappings.is_empty() {
        return Ok(());
    }

    let mut directories = vec![bundle_path.to_path_buf()];
    let mut patched_files = 0usize;
    let mut patched_references = 0usize;

    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory)
            .context(format!("Failed to read {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                if entry.file_name() != "_CodeSignature" {
                    directories.push(path);
                }
                continue;
            }

            if !file_type.is_file() || entry.file_name() == "embedded.mobileprovision" {
                continue;
            }

            let mut data =
                std::fs::read(&path).context(format!("Failed to read {}", path.display()))?;
            let mut references_in_file = 0usize;

            for (original, replacement) in mappings {
                references_in_file +=
                    replace_equal_length(&mut data, original.as_bytes(), replacement.as_bytes());
            }

            if references_in_file == 0 {
                continue;
            }

            std::fs::write(&path, data).context(format!("Failed to patch {}", path.display()))?;
            patched_files += 1;
            patched_references += references_in_file;
            info!(
                "Patched {} app group reference(s) in {}",
                references_in_file,
                path.display()
            );
        }
    }

    if patched_references == 0 {
        bail!(
            "No embedded references found for original app groups in {}",
            bundle_path.display()
        );
    }

    info!(
        "Patched {} app group reference(s) across {} file(s)",
        patched_references, patched_files
    );
    Ok(())
}

fn replace_equal_length(data: &mut [u8], original: &[u8], replacement: &[u8]) -> usize {
    assert_eq!(original.len(), replacement.len());
    if original.is_empty() {
        return 0;
    }

    let mut replacements = 0usize;
    let mut offset = 0usize;
    while offset + original.len() <= data.len() {
        if data[offset..].starts_with(original) {
            data[offset..offset + original.len()].copy_from_slice(replacement);
            replacements += 1;
            offset += original.len();
        } else {
            offset += 1;
        }
    }

    replacements
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

fn validate_profile_identity(
    entitlements: &Dictionary,
    expected_bundle_id: &str,
    team: &DeveloperTeam,
    bundle_path: &Path,
) -> Result<(), Report> {
    let application_identifier = entitlements
        .get("application-identifier")
        .and_then(plist::Value::as_string)
        .ok_or_report()
        .context(format!(
            "Provisioning profile for {} has no application identifier",
            bundle_path.display()
        ))?;
    let expected_suffix = format!(".{expected_bundle_id}");
    if !application_identifier.ends_with(&expected_suffix) {
        bail!(
            "Provisioning profile for {} belongs to {}, expected {}",
            bundle_path.display(),
            application_identifier,
            expected_bundle_id
        );
    }

    let profile_team_id = entitlements
        .get("com.apple.developer.team-identifier")
        .and_then(plist::Value::as_string)
        .ok_or_report()
        .context(format!(
            "Provisioning profile for {} has no team identifier",
            bundle_path.display()
        ))?;
    if profile_team_id != team.team_id {
        bail!(
            "Provisioning profile for {} belongs to team {}, expected {}",
            bundle_path.display(),
            profile_team_id,
            team.team_id
        );
    }

    Ok(())
}

fn validate_signed_bundle(
    bundle_path: &Path,
    expected_bundle_id: &str,
    team: &DeveloperTeam,
    requested_app_groups: &[String],
) -> Result<(), Report> {
    let bundle = Bundle::new(bundle_path.to_path_buf())?;
    let actual_bundle_id = bundle.bundle_identifier().ok_or_report().context(format!(
        "Signed bundle {} has no bundle identifier",
        bundle_path.display()
    ))?;
    if actual_bundle_id != expected_bundle_id {
        bail!(
            "Signed bundle {} has bundle identifier {}, expected {}",
            bundle_path.display(),
            actual_bundle_id,
            expected_bundle_id
        );
    }

    let entitlements = code_signature_entitlements(&bundle)?
        .ok_or_report()
        .context(format!(
            "Signed bundle {} has no signed entitlements",
            bundle_path.display()
        ))?;
    validate_profile_identity(&entitlements, expected_bundle_id, team, bundle_path)?;
    validate_requested_app_groups(&entitlements, requested_app_groups, bundle_path)?;
    info!(
        "Verified signed bundle {} with shared app groups: {}",
        expected_bundle_id,
        requested_app_groups.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        app_groups_from_entitlements, nested_code_seal, remap_app_groups, replace_equal_length,
        required_app_groups, validate_requested_app_groups,
    };
    use isideload::sideload::application::SpecialApp;
    use plist::{Dictionary, Value};
    use std::path::Path;

    #[test]
    fn preserves_original_app_groups() {
        let mut entitlements = Dictionary::new();
        entitlements.insert(
            "com.apple.security.application-groups".to_string(),
            Value::Array(vec![
                Value::String("group.de.marxon.wedee".to_string()),
                Value::String("group.de.marxon.shared".to_string()),
            ]),
        );

        assert_eq!(
            app_groups_from_entitlements(&entitlements),
            vec![
                "group.de.marxon.wedee".to_string(),
                "group.de.marxon.shared".to_string()
            ]
        );
    }

    #[test]
    fn ignores_missing_original_app_groups() {
        assert!(app_groups_from_entitlements(&Dictionary::new()).is_empty());
    }

    #[test]
    fn remaps_app_groups_without_changing_length() {
        let mappings =
            remap_app_groups(&["group.de.marxon.wedee".to_string()], "4CP62AN6Z9").unwrap();

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "group.de.marxon.wedee");
        assert_eq!(mappings[0].0.len(), mappings[0].1.len());
        assert!(mappings[0].1.starts_with("group."));
        assert_ne!(mappings[0].0, mappings[0].1);
    }

    #[test]
    fn app_group_remapping_depends_on_team() {
        let app_groups = ["group.de.marxon.wedee".to_string()];

        let first = remap_app_groups(&app_groups, "4CP62AN6Z9").unwrap();
        let second = remap_app_groups(&app_groups, "ABCDEFGHIJ").unwrap();

        assert_ne!(first[0].1, second[0].1);
    }

    #[test]
    fn remaps_longer_app_groups_first() {
        let mappings = remap_app_groups(
            &[
                "group.example".to_string(),
                "group.example.widgets".to_string(),
            ],
            "4CP62AN6Z9",
        )
        .unwrap();

        assert_eq!(mappings[0].0, "group.example.widgets");
        assert_eq!(mappings[1].0, "group.example");
    }

    #[test]
    fn replaces_all_equal_length_app_group_references() {
        let mut data = b"group.example bytes group.example".to_vec();

        let replacements = replace_equal_length(&mut data, b"group.example", b"group.1234567");

        assert_eq!(replacements, 2);
        assert_eq!(data, b"group.1234567 bytes group.1234567");
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

    #[test]
    fn requires_default_app_group_for_unsigned_app() {
        let groups = required_app_groups(&[], "de.marxon.wedee.4CP62AN6Z9", &None, "4CP62AN6Z9");

        assert_eq!(groups, vec!["group.de.marxon.wedee.4CP62AN6Z9".to_string()]);
    }

    #[test]
    fn requires_remapped_and_default_app_groups() {
        let groups = required_app_groups(
            &[(
                "group.de.marxon.wedee".to_string(),
                "group.0123456789abcdef".to_string(),
            )],
            "de.marxon.wedee.4CP62AN6Z9",
            &None,
            "4CP62AN6Z9",
        );

        assert_eq!(
            groups,
            vec![
                "group.0123456789abcdef".to_string(),
                "group.de.marxon.wedee.4CP62AN6Z9".to_string(),
            ]
        );
    }

    #[test]
    fn preserves_sidestore_live_container_app_group() {
        let groups = required_app_groups(
            &[],
            "com.SideStore.SideStore.4CP62AN6Z9",
            &Some(SpecialApp::SideStoreLc),
            "4CP62AN6Z9",
        );

        assert_eq!(
            groups,
            vec!["group.com.SideStore.SideStore.4CP62AN6Z9".to_string()]
        );
    }

    #[test]
    fn accepts_nested_app_extension_code_seal() {
        let mut seal = Dictionary::new();
        seal.insert("cdhash".to_string(), Value::Data(vec![1, 2, 3]));
        let mut files2 = Dictionary::new();
        files2.insert(
            "PlugIns/WedeeWidgets.appex".to_string(),
            Value::Dictionary(seal),
        );

        assert_eq!(
            nested_code_seal(&files2, "PlugIns/WedeeWidgets.appex"),
            Some([1, 2, 3].as_slice())
        );
    }

    #[test]
    fn rejects_regular_file_hash_for_app_extension() {
        let mut seal = Dictionary::new();
        seal.insert("hash2".to_string(), Value::Data(vec![1, 2, 3]));
        let mut files2 = Dictionary::new();
        files2.insert(
            "PlugIns/WedeeWidgets.appex/WedeeWidgets".to_string(),
            Value::Dictionary(seal),
        );

        assert_eq!(
            nested_code_seal(&files2, "PlugIns/WedeeWidgets.appex"),
            None
        );
    }
}
