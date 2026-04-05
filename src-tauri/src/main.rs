// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        use std::env;
        if env::var_os("__NV_DISABLE_EXPLICIT_SYNC").is_none() {
            unsafe {
                env::set_var("__NV_DISABLE_EXPLICIT_SYNC", "1");
            }
        }
    }

    // To be quite honest, I have no idea how ring has made its way into the dependency tree in some cases.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    isideload::init().expect("Failed to initialize error reporting");
    iloader_lib::run()
}
