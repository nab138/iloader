use isideload::{
    anisette::remote_v3::RemoteV3AnisetteProvider,
    auth::apple_account::AppleAccount,
    dev::{
        app_ids::{AppIdsApi, ListAppIdsResponse},
        certificates::{CertificatesApi, DevelopmentCertificate},
        developer_session::DeveloperSession,
    },
    sideload::{SideloaderBuilder, builder::MaxCertsBehavior, sideloader::Sideloader},
};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Listener, State, Window};
use tauri_plugin_store::StoreExt;
use tracing::debug;

use crate::{
    secure_storage::create_sideloading_storage,
    sideload::{SideloaderGuard, SideloaderMutex},
};

#[tauri::command]
pub async fn login_new(
    handle: AppHandle,
    window: Window,
    sideloader_state: State<'_, SideloaderMutex>,
    email: String,
    password: String,
    anisette_server: String,
    save_credentials: bool,
) -> Result<(), String> {
    let account = login(&handle, &window, &email, &password, anisette_server).await?;
    let mut sideloader_guard = sideloader_state.lock().unwrap();
    *sideloader_guard = Some(account);

    if save_credentials {
        let pass_entry = Entry::new("iloader", &email)
            .map_err(|e| format!("Failed to create keyring entry for credentials: {:?}.", e))?;
        pass_entry
            .set_password(&password)
            .map_err(|e| format!("Failed to save credentials to keyring: {:?}", e))?;
        let store = handle
            .store("data.json")
            .map_err(|e| format!("Failed to get store: {:?}", e))?;
        let mut existing_ids = store
            .get("ids")
            .unwrap_or_else(|| Value::Array(vec![]))
            .as_array()
            .cloned()
            .unwrap_or_else(std::vec::Vec::new);
        let value = Value::String(email.clone());
        if !existing_ids.contains(&value) {
            existing_ids.push(value);
        }
        store.set("ids", Value::Array(existing_ids));
    }
    Ok(())
}

#[tauri::command]
pub async fn login_stored(
    handle: AppHandle,
    window: Window,
    email: String,
    anisette_server: String,
    sideloader_state: State<'_, SideloaderMutex>,
) -> Result<(), String> {
    let pass_entry = Entry::new("iloader", &email)
        .map_err(|e| format!("Failed to create keyring entry for credentials: {:?}.", e))?;
    let password = pass_entry
        .get_password()
        .map_err(|e| format!("Failed to get credentials: {:?}", e))?;
    let account = login(&handle, &window, &email, &password, anisette_server).await?;
    let mut sideloader_guard = sideloader_state.lock().unwrap();
    *sideloader_guard = Some(account);

    Ok(())
}

#[tauri::command]
pub fn delete_account(handle: AppHandle, email: String) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();

    match Entry::new("iloader", &email) {
        Ok(pass_entry) => {
            if let Err(e) = pass_entry.delete_credential() {
                errors.push(format!("Failed to delete credentials: {:?}.", e));
            }
        }
        Err(e) => errors.push(format!(
            "Failed to create keyring entry for credentials: {:?}.",
            e
        )),
    }

    match handle.store("data.json") {
        Ok(store) => {
            let mut existing_ids = store
                .get("ids")
                .unwrap_or_else(|| Value::Array(vec![]))
                .as_array()
                .cloned()
                .unwrap_or_else(std::vec::Vec::new);
            existing_ids.retain(|v| v.as_str().is_none_or(|s| s != email));
            store.set("ids", Value::Array(existing_ids));
        }
        Err(e) => errors.push(format!("Failed to get store: {:?}.", e)),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

#[tauri::command]
pub fn logged_in_as(sideloader_state: State<'_, SideloaderMutex>) -> Option<String> {
    let sideloader_guard = sideloader_state.lock().unwrap();
    if let Some(account) = &*sideloader_guard {
        return Some(account.get_email().to_string());
    }
    None
}

#[tauri::command]
pub fn invalidate_account(sideloader_state: State<'_, SideloaderMutex>) {
    let mut sideloader_guard = sideloader_state.lock().unwrap();
    *sideloader_guard = None;
}

#[tauri::command]
pub fn reset_anisette_state() -> Result<bool, String> {
    let state_entry = Entry::new("iloader", "anisette_state")
        .map_err(|e| format!("Failed to create keyring entry for anisette: {:?}.", e))?;

    match state_entry.delete_credential() {
        Ok(_) => {
            debug!("Anisette state deleted from keyring.");
            Ok(true)
        }
        Err(keyring::Error::NoEntry) => {
            debug!("No existing anisette state found in keyring, nothing to delete.");
            Ok(false)
        }
        Err(e) => Err(format!("Failed to delete anisette state: {:?}", e)),
    }
}

async fn login(
    app: &AppHandle,
    window: &Window,
    email: &str,
    password: &str,
    anisette_server: String,
) -> Result<Sideloader, String> {
    let window_clone = window.clone();
    let tfa_closure = move || -> Option<String> {
        window_clone
            .emit("2fa-required", ())
            .expect("Failed to emit 2fa-required event");

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let handler_id = window_clone.listen("2fa-recieved", move |event| {
            let code = event.payload();
            let _ = tx.send(code.to_string());
        });

        let result = rx.recv_timeout(Duration::from_secs(120));
        window_clone.unlisten(handler_id);

        match result {
            Ok(code) => {
                let code = code.trim_matches('"').to_string();
                Some(code)
            }
            Err(_) => None,
        }
    };

    let anisette_url = if !anisette_server.starts_with("http") {
        format!("https://{}", anisette_server)
    } else {
        anisette_server
    };

    let mut account = AppleAccount::builder(&email.to_lowercase())
        .anisette_provider(
            RemoteV3AnisetteProvider::default()
                .set_serial_number("0".to_string())
                .set_storage(create_sideloading_storage(app)?)
                .set_url(&anisette_url),
        )
        .login(password, tfa_closure)
        .await
        .map_err(|e| e.to_string())?;

    debug!("Logged in");

    let dev_session = DeveloperSession::from_account(&mut account)
        .await
        .map_err(|e| e.to_string())?;

    debug!("Created developer session");

    let max_certs_callback = {
        let window_clone = window.clone();
        move |certs: &Vec<DevelopmentCertificate>| -> Option<Vec<String>> {
            let cert_infos: Vec<CertificateInfo> = certs
                .iter()
                .map(|cert| CertificateInfo {
                    name: cert.name.clone(),
                    certificate_id: cert.certificate_id.clone(),
                    serial_number: cert.serial_number.clone(),
                    machine_name: cert.machine_name.clone(),
                    machine_id: cert.machine_id.clone(),
                })
                .collect();
            window_clone
                .emit("max-certs-reached", cert_infos)
                .expect("Failed to emit max-certs-reached event");

            let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<String>>>();
            let handler_id = window_clone.listen("max-certs-response", move |event| {
                let certs = event.payload();
                let certs = serde_json::from_str::<Option<Vec<String>>>(certs).unwrap_or(None);
                let _ = tx.send(certs);
            });

            let result = rx.recv_timeout(Duration::from_secs(300));
            window_clone.unlisten(handler_id);
            result.unwrap_or(None)
        }
    };

    // TODO: Team Selection

    let sideloader = SideloaderBuilder::new(dev_session, email.to_lowercase())
        .machine_name("iloader".to_string())
        .storage(create_sideloading_storage(app)?)
        .max_certs_behavior(MaxCertsBehavior::Prompt(Box::new(max_certs_callback)))
        .build();

    debug!("Built sideloader");

    Ok(sideloader)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateInfo {
    pub name: Option<String>,
    pub certificate_id: Option<String>,
    pub serial_number: Option<String>,
    pub machine_name: Option<String>,
    pub machine_id: Option<String>,
}

#[tauri::command]
pub async fn get_certificates(
    sideloader_state: State<'_, SideloaderMutex>,
) -> Result<Vec<CertificateInfo>, String> {
    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    let team = sideloader
        .get_mut()
        .get_team()
        .await
        .map_err(|e| e.to_string())?;
    let dev_session = sideloader.get_mut().get_dev_session();

    let certificates = dev_session
        .list_all_development_certs(&team, None)
        .await
        .map_err(|e| format!("Failed to get development certificates: {:?}.", e))?;

    Ok(certificates
        .into_iter()
        .map(|cert| CertificateInfo {
            name: cert.name,
            certificate_id: cert.certificate_id,
            serial_number: cert.serial_number,
            machine_name: cert.machine_name,
            machine_id: cert.machine_id,
        })
        .collect())
}

#[tauri::command]
pub async fn revoke_certificate(
    serial_number: String,
    sideloader_state: State<'_, SideloaderMutex>,
) -> Result<(), String> {
    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    let team = sideloader
        .get_mut()
        .get_team()
        .await
        .map_err(|e| e.to_string())?;
    let dev_session = sideloader.get_mut().get_dev_session();

    dev_session
        .revoke_development_cert(&team, &serial_number, None)
        .await
        .map_err(|e| format!("Failed to revoke development certificates: {:?}.", e))?;

    Ok(())
}

#[tauri::command]
pub async fn list_app_ids(
    sideloader_state: State<'_, SideloaderMutex>,
) -> Result<ListAppIdsResponse, String> {
    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    let team = sideloader
        .get_mut()
        .get_team()
        .await
        .map_err(|e| e.to_string())?;
    let dev_session = sideloader.get_mut().get_dev_session();

    let response = dev_session
        .list_app_ids(&team, None)
        .await
        .map_err(|e| e.to_string())?;

    Ok(response.clone())
}

#[tauri::command]
pub async fn delete_app_id(
    app_id_id: String,
    sideloader_state: State<'_, SideloaderMutex>,
) -> Result<(), String> {
    let mut sideloader = SideloaderGuard::take(&sideloader_state)?;

    let team = sideloader
        .get_mut()
        .get_team()
        .await
        .map_err(|e| e.to_string())?;
    let dev_session = sideloader.get_mut().get_dev_session();

    dev_session
        .delete_app_id(&team, &app_id_id, None)
        .await
        .map_err(|e| format!("Failed to delete App ID: {:?}.", e))?;

    Ok(())
}
