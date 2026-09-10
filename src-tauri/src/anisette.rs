use isideload::{
    anisette::{
        AnisetteClientInfo, AnisetteData, AnisetteProvider, remote_v3::RemoteV3AnisetteProvider,
    },
    auth::grandslam::GrandSlam,
};
use rootcause::prelude::*;
use std::sync::Arc;
use tracing::debug;

/// Client info sent to Apple's GrandSlam servers as `X-Mme-Client-Info`.
///
/// Format: `<hardware model> <os;version;build> <com.apple.AuthKit/1 (<bundle id>/<version>)>`
///
/// We identify as `akd` (Apple's authentication daemon) rather than Xcode, the same way
/// AltServer does. Apple treats Xcode-identified auth requests more strictly, which shows
/// up as spurious 2FA loops or rejected logins on some Apple IDs.
pub const AKD_CLIENT_INFO: &str =
    "<Mac15,7> <macOS;27.0;26A5378j> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";

/// The client info isideload uses by default. Kept selectable as a fallback, since Apple
/// occasionally does the opposite and rejects clients that don't look like Xcode.
///
/// Selected from the settings UI rather than from Rust, hence the allow.
#[allow(dead_code)]
pub const XCODE_CLIENT_INFO: &str =
    "<Mac15,7> <macOS;27.0;26A5378j> <com.apple.AuthKit/1 (com.apple.dt.Xcode/25183.54.10)>";

/// User agent paired with the client info above.
pub const DEFAULT_USER_AGENT: &str = "akd/1.0 CFNetwork/808.1.4";

/// Wraps isideload's remote anisette provider so the client info it reports can be changed
/// without forking the crate. Everything else is delegated to the inner provider.
pub struct IloaderAnisetteProvider {
    inner: RemoteV3AnisetteProvider,
    client_info: AnisetteClientInfo,
}

impl IloaderAnisetteProvider {
    /// `client_info` overrides the advertised client info; an empty or missing value falls
    /// back to [`AKD_CLIENT_INFO`].
    pub fn new(inner: RemoteV3AnisetteProvider, client_info: Option<String>) -> Self {
        let client_info = client_info
            .map(|info| info.trim().to_string())
            .filter(|info| !info.is_empty())
            .unwrap_or_else(|| AKD_CLIENT_INFO.to_string());

        debug!("Using anisette client info: {}", client_info);

        Self {
            inner,
            client_info: AnisetteClientInfo {
                client_info,
                user_agent: DEFAULT_USER_AGENT.to_string(),
            },
        }
    }
}

#[async_trait::async_trait]
impl AnisetteProvider for IloaderAnisetteProvider {
    async fn get_anisette_data(&self) -> Result<AnisetteData, Report> {
        self.inner.get_anisette_data().await
    }

    async fn get_client_info(&self) -> Result<AnisetteClientInfo, Report> {
        Ok(self.client_info.clone())
    }

    async fn provision(&mut self, gs: Arc<GrandSlam>) -> Result<(), Report> {
        self.inner.provision(gs).await
    }

    fn needs_provisioning(&self) -> Result<bool, Report> {
        self.inner.needs_provisioning()
    }
}
