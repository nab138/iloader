use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use isideload::util::{
    fs_storage::FsStorage, keyring_storage::KeyringStorage, storage::SideloadingStorage,
};
use tauri::{AppHandle, Manager};
use tracing::warn;

use crate::error::AppError;

static FORCE_DISABLE_KEYRING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub fn force_disable_keyring(force: bool) {
    FORCE_DISABLE_KEYRING.store(force, Ordering::Relaxed);

    if force {
        warn!("Keyring has been forcefully disabled by the user.");
    } else {
        let available = check_keyring_available();
        if !available {
            warn!("Keyring is not available and cannot be enabled.");
        }
    }
}

#[tauri::command]
pub fn keyring_available() -> bool {
    !FORCE_DISABLE_KEYRING.load(Ordering::Relaxed) && check_keyring_available()
}

/// Probe whether the OS keychain is usable, **once per process**. The probe does a
/// real `get_password`, which on macOS pops the "iloader wants to use your confidential
/// information" prompt. This function is called on nearly every storage access
/// (`create_sideloading_storage`, `with_pairing_storage`, the frontend keyring check),
/// so without caching the prompt appeared 4+ times per launch (and again mid-install).
/// Keychain availability doesn't change during a session, so caching the result is
/// safe and collapses those to a single prompt. (User toggling is handled separately
/// by `FORCE_DISABLE_KEYRING`, which doesn't touch this probe.)
fn check_keyring_available() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let entry = keyring::Entry::new("iloader", "test");
        if let Ok(entry) = entry {
            entry.set_password("test").is_ok() && entry.get_password().is_ok()
        } else {
            false
        }
    })
}

pub fn create_sideloading_storage(
    app: &AppHandle,
) -> Result<Box<dyn SideloadingStorage>, AppError> {
    if keyring_available() {
        Ok(Box::new(KeyringStorage::new("iloader".to_string())))
    } else {
        warn!(
            "Keyring is not available, falling back to filesystem storage for sideloading data. This is insecure!"
        );
        Ok(Box::new(FsStorage::new(
            app.path().app_data_dir().map_err(|e| {
                AppError::Misc(format!("Failed to get app data directory: {:?}", e))
            })?,
        )))
    }
}
