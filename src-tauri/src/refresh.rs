use std::collections::{HashMap, HashSet};

use idevice::{
    IdeviceService, installation_proxy::InstallationProxyClient, misagent::MisagentClient,
};
use isideload::dev::app_ids::AppIdsApi;
use serde::Serialize;
use tauri::{State, Window};

use crate::{
    device::{DeviceInfoMutex, get_provider},
    error::AppError,
    operation::Operation,
    sideload::{SideloaderGuard, SideloaderMutex},
};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SideStoreApp {
    pub name: String,
    pub bundle_id: String,
    /// Bundle id stripped of trailing `.<TEAM_ID>` if present.
    pub base_bundle_id: String,
    pub team_identifier: Option<String>,
    pub signer_identity: Option<String>,
    pub version: Option<String>,
    /// Whether this app appears to be sideloaded (has a SignerIdentity, not a system app).
    pub is_sideloaded: bool,
}

fn get_string(dict: &plist::Dictionary, key: &str) -> Option<String> {
    dict.get(key).and_then(|v| v.as_string()).map(String::from)
}

fn extract_team_id(app: &plist::Dictionary) -> Option<String> {
    if let Some(t) = get_string(app, "TeamIdentifier") {
        return Some(t);
    }
    if let Some(plist::Value::Array(arr)) = app.get("TeamIdentifier")
        && let Some(plist::Value::String(s)) = arr.first()
    {
        return Some(s.clone());
    }
    if let Some(ent) = app.get("Entitlements").and_then(|v| v.as_dictionary()) {
        if let Some(t) = get_string(ent, "com.apple.developer.team-identifier") {
            return Some(t);
        }
        // application-identifier is "<TEAM_ID>.<bundle_id>"
        if let Some(app_id) = get_string(ent, "application-identifier")
            && let Some((team, _)) = app_id.split_once('.')
        {
            return Some(team.to_string());
        }
    }
    // SignerIdentity often looks like: "iPhone Developer: name (TEAMID)"
    if let Some(signer) = get_string(app, "SignerIdentity")
        && let Some(start) = signer.rfind('(')
        && let Some(end) = signer.rfind(')')
        && end > start + 1
    {
        let candidate = &signer[start + 1..end];
        if candidate.len() == 10 && candidate.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn strip_team_suffix(bundle_id: &str, team_id: Option<&str>) -> String {
    if let Some(team_id) = team_id {
        let suffix = format!(".{}", team_id);
        if bundle_id.ends_with(&suffix) {
            return bundle_id[..bundle_id.len() - suffix.len()].to_string();
        }
    }
    bundle_id.to_string()
}

async fn fetch_apps_inner(
    device_state: &State<'_, DeviceInfoMutex>,
) -> Result<Vec<SideStoreApp>, AppError> {
    let device = {
        let guard = device_state.lock().unwrap();
        match &*guard {
            Some(d) => d.clone(),
            None => return Err(AppError::NoDeviceSelected),
        }
    };

    let provider = get_provider(&device.info).await?;
    let mut inst = InstallationProxyClient::connect(&provider).await.map_err(|e| {
        AppError::DeviceComsWithMessage(
            "Failed to connect to installation proxy".into(),
            e.to_string(),
        )
    })?;

    let installed = inst.get_apps(Some("User"), None).await.map_err(|e| {
        AppError::DeviceComsWithMessage("Failed to list installed apps".into(), e.to_string())
    })?;

    let mut result = Vec::new();
    for (bundle_id, app_value) in installed {
        let Some(app) = app_value.as_dictionary() else {
            continue;
        };

        let name = get_string(app, "CFBundleDisplayName")
            .or_else(|| get_string(app, "CFBundleName"))
            .unwrap_or_else(|| bundle_id.clone());
        let signer = get_string(app, "SignerIdentity");
        let team_id = extract_team_id(app);
        let version = get_string(app, "CFBundleShortVersionString");

        // SideStore-installed app markers — at least one of these must be present:
        //  - ALTBundleIdentifier (set by SideStore at install time)
        //  - a GroupContainers / ALTAppGroups entry referencing AltStore/SideStore app groups
        //  - UTExportedTypeDeclarations entry with "io.sidestore.Installed.*" identifier
        let has_alt_bundle_id = app.get("ALTBundleIdentifier").is_some();
        let has_alt_app_groups = app.get("ALTAppGroups").is_some();

        let group_containers_match = app
            .get("GroupContainers")
            .and_then(|v| v.as_dictionary())
            .map(|d| {
                d.keys().any(|k| {
                    k.contains("com.SideStore.SideStore") || k.contains("com.rileytestut.AltStore")
                })
            })
            .unwrap_or(false);

        let utt_exports_sidestore = app
            .get("UTExportedTypeDeclarations")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|entry| {
                    entry
                        .as_dictionary()
                        .and_then(|d| d.get("UTTypeIdentifier"))
                        .and_then(|v| v.as_string())
                        .map(|s| s.starts_with("io.sidestore.Installed."))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        let is_sidestore_app = has_alt_bundle_id
            || has_alt_app_groups
            || group_containers_match
            || utt_exports_sidestore;
        if !is_sidestore_app {
            continue;
        }

        let base_bundle_id = strip_team_suffix(&bundle_id, team_id.as_deref());

        result.push(SideStoreApp {
            name,
            bundle_id,
            base_bundle_id,
            team_identifier: team_id,
            signer_identity: signer,
            version,
            is_sideloaded: true,
        });
    }

    result.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(result)
}

#[tauri::command]
pub async fn list_sidestore_apps(
    device_state: State<'_, DeviceInfoMutex>,
) -> Result<Vec<SideStoreApp>, AppError> {
    fetch_apps_inner(&device_state).await
}

async fn refresh_one(
    device_state: &State<'_, DeviceInfoMutex>,
    sideloader_state: &State<'_, SideloaderMutex>,
    bundle_id: &str,
) -> Result<(), AppError> {
    let device = {
        let guard = device_state.lock().unwrap();
        match &*guard {
            Some(d) => d.clone(),
            None => return Err(AppError::NoDeviceSelected),
        }
    };

    let provider = get_provider(&device.info).await?;

    // Acquire team + matching app id from the developer session.
    let mut sideloader = SideloaderGuard::take(sideloader_state)?;
    let team = sideloader.get_mut().get_team().await?;
    let dev_session = sideloader.get_mut().get_dev_session();

    let app_ids_resp = dev_session
        .list_app_ids(&team, None)
        .await
        .map_err(AppError::from)?;

    // Find the matching App ID. Apple stores it as "<base>.<team_id>". The
    // installed app's bundle_id is also "<base>.<team_id>" most of the time,
    // but we accept exact match against either form.
    let target_id = app_ids_resp
        .app_ids
        .iter()
        .find(|a| a.identifier == bundle_id)
        .or_else(|| {
            app_ids_resp
                .app_ids
                .iter()
                .find(|a| a.identifier.starts_with(&format!("{}.", bundle_id)))
        })
        .ok_or_else(|| {
            AppError::Misc(format!(
                "Could not find a matching App ID on the developer account for {}. \
                 The app must have been installed with this Apple ID.",
                bundle_id
            ))
        })?
        .clone();

    let profile = dev_session
        .download_team_provisioning_profile(&team, &target_id, None)
        .await
        .map_err(AppError::from)?;

    let profile_bytes: Vec<u8> = profile.encoded_profile.as_ref().to_vec();

    drop(sideloader);

    let mut misagent = MisagentClient::connect(&provider).await.map_err(|e| {
        AppError::DeviceComsWithMessage("Failed to connect to misagent".into(), e.to_string())
    })?;

    misagent.install(profile_bytes).await.map_err(|e| {
        AppError::DeviceComsWithMessage(
            "Failed to install provisioning profile via misagent".into(),
            e.to_string(),
        )
    })?;

    Ok(())
}

#[tauri::command]
pub async fn refresh_sidestore_app_operation(
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
    bundle_id: String,
) -> Result<(), AppError> {
    let op = Operation::new("refresh_sidestore_app".to_string(), &window);
    op.start("refresh")?;
    op.fail_if_err(
        "refresh",
        refresh_one(&device_state, &sideloader_state, &bundle_id).await,
    )?;
    op.complete("refresh")?;
    Ok(())
}

#[tauri::command]
pub async fn refresh_all_sidestore_apps_operation(
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    sideloader_state: State<'_, SideloaderMutex>,
) -> Result<RefreshAllResult, AppError> {
    let op = Operation::new("refresh_all_sidestore_apps".to_string(), &window);
    op.start("collect")?;

    let apps = op.fail_if_err("collect", fetch_apps_inner(&device_state).await)?;
    op.move_on("collect", "refresh")?;

    // Determine team id once (used for filtering apps to refresh).
    let team_id = {
        let mut sideloader = match SideloaderGuard::take(&sideloader_state) {
            Ok(s) => s,
            Err(e) => return op.fail("refresh", e),
        };
        match sideloader.get_mut().get_team().await {
            Ok(t) => Some(t.team_id),
            Err(_) => None,
        }
    };

    let mut succeeded = Vec::new();
    let mut failed: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for app in apps {
        if let (Some(t1), Some(t2)) = (team_id.as_ref(), app.team_identifier.as_ref())
            && t1 != t2
        {
            // Skip apps signed with a different team than the one we're logged into.
            continue;
        }
        if !seen.insert(app.bundle_id.clone()) {
            continue;
        }

        match refresh_one(&device_state, &sideloader_state, &app.bundle_id).await {
            Ok(()) => succeeded.push(app.bundle_id.clone()),
            Err(e) => {
                failed.insert(app.bundle_id.clone(), e.to_string());
            }
        }
    }

    op.complete("refresh")?;

    Ok(RefreshAllResult { succeeded, failed })
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RefreshAllResult {
    pub succeeded: Vec<String>,
    pub failed: HashMap<String, String>,
}
