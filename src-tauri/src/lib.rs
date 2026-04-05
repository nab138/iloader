#[macro_use]
mod account;
#[macro_use]
mod device;
#[macro_use]
mod sideload;
#[macro_use]
mod pairing;
#[macro_use]
mod secure_storage;
mod logging;
mod operation;

use crate::{
    account::{
        delete_account, delete_app_id, get_certificates, invalidate_account, list_app_ids,
        logged_in_as, login_new, login_stored, reset_anisette_state, revoke_certificate,
    },
    device::{
        DeviceInfoMutex, PairingCancelToken, cancel_pairing, list_devices, set_selected_device,
    },
    pairing::{
        delete_stored_rppairing, export_pairing_cmd, installed_pairing_apps, place_pairing_cmd,
    },
    secure_storage::{force_disable_keyring, keyring_available},
    sideload::{SideloaderMutex, install_sidestore_operation, sideload_operation},
};
use tauri::Manager;
use tracing_subscriber::{Layer, Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .setup(|app| {
            let log_dir = app
                .path()
                .app_data_dir()
                .expect("failed to get app data dir")
                .join("logs");

            std::fs::create_dir_all(&log_dir).ok();

            let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("iloader")
                .filename_suffix("log")
                .max_log_files(2)
                .build(&log_dir)
                .expect("failed to create log file appender");

            let file_layer = fmt::layer()
                .with_writer(file_appender)
                .with_target(true)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG);

            let frontend_layer = logging::FrontendLoggingLayer::new(app.handle().clone())
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG);

            Registry::default()
                .with(file_layer)
                .with(frontend_layer)
                .init();

            std::panic::set_hook(Box::new(|panic_info| {
                let thread = std::thread::current();
                let thread_name = thread.name().unwrap_or("<unnamed>");

                let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
                    s.clone()
                } else {
                    "<non-string panic payload>".to_string()
                };

                let location = panic_info
                    .location()
                    .map(|loc| format!("{}:{}", loc.file(), loc.line()))
                    .unwrap_or_else(|| "<unknown>".to_string());

                let backtrace = std::backtrace::Backtrace::capture();

                tracing::error!(
                    target: "panic",
                    thread = thread_name,
                    location = location,
                    message = message,
                    backtrace = %backtrace,
                    "panic captured"
                );
            }));

            app.manage(DeviceInfoMutex::new(None));
            app.manage(SideloaderMutex::new(None));
            app.manage(PairingCancelToken::new(None));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            login_new,
            invalidate_account,
            logged_in_as,
            login_stored,
            delete_account,
            list_devices,
            sideload_operation,
            set_selected_device,
            install_sidestore_operation,
            get_certificates,
            revoke_certificate,
            list_app_ids,
            delete_app_id,
            installed_pairing_apps,
            place_pairing_cmd,
            reset_anisette_state,
            export_pairing_cmd,
            delete_stored_rppairing,
            keyring_available,
            force_disable_keyring,
            cancel_pairing
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
