//! Manual GSA sign-in probe. Run with:
//!   ILOADER_TEST_EMAIL=... ILOADER_TEST_PASSWORD=... cargo test --test gsa_signin -- --ignored --nocapture
//! With no password set it still performs the SRP init request, which is where the
//! X-MMe-Client-Info 503 block used to fire.
use isideload::{
    anisette::remote_v3::RemoteV3AnisetteProvider,
    auth::apple_account::{AppleAccount, TwoFactorCallbackResponse},
    util::fs_storage::FsStorage,
};

#[tokio::test]
#[ignore]
async fn gsa_signin_probe() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    isideload::init().expect("Failed to initialize error reporting");
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(true)
        .init();

    let email = std::env::var("ILOADER_TEST_EMAIL").unwrap_or_else(|_| "austin@archibalds.tv".into());
    let password = std::env::var("ILOADER_TEST_PASSWORD")
        .unwrap_or_else(|_| "gsa-503-probe-not-a-real-password".into());
    let store_dir = std::env::var("ILOADER_TEST_STORE").unwrap_or_else(|_| "/tmp/iloader-gsa-probe".into());

    let provider = RemoteV3AnisetteProvider::default()
        .unwrap()
        .set_storage(Box::new(FsStorage::new(store_dir.into())))
        .set_url("https://ani.sidestore.io");

    let result = AppleAccount::builder(&email)
        .anisette_provider(provider)
        .login(&password, |params| async move {
            println!("### 2FA REQUIRED: {params:?}");
            Ok(TwoFactorCallbackResponse::SubmitCode("000000".into()))
        })
        .await;

    match result {
        Ok(_) => println!("### SIGN-IN OK"),
        Err(e) => println!("### SIGN-IN FAILED: {e:?}"),
    }
}
