use std::sync::Arc;

use rootcause::prelude::*;
use tokio::sync::RwLock;

use crate::{
    anisette::{AnisetteDataGenerator, AnisetteProvider, remote_v3::RemoteV3AnisetteProvider},
    auth::apple_account::{AppleAccount, TwoFactorCallbackParams, TwoFactorCallbackResponse},
};

pub struct AppleAccountBuilder {
    email: String,
    debug: Option<bool>,
    anisette_generator: Option<AnisetteDataGenerator>,
    proxy_url: Option<String>,
}

impl AppleAccountBuilder {
    /// Create a new AppleAccountBuilder with the given email
    ///
    /// # Arguments
    /// - `email`: The Apple ID email address
    pub fn new(email: &str) -> Self {
        Self {
            email: email.to_string(),
            debug: None,
            anisette_generator: None,
            proxy_url: None,
        }
    }

    /// DANGER Set whether to enable debug mode
    ///
    /// # Arguments
    /// - `debug`: If true, accept invalid certificates and enable verbose connection logging
    pub fn danger_debug(mut self, debug: bool) -> Self {
        self.debug = Some(debug);
        self
    }

    pub fn proxy_url(mut self, proxy_url: String) -> Self {
        self.proxy_url = Some(proxy_url);
        self
    }

    pub fn anisette_provider(
        mut self,
        anisette_provider: impl AnisetteProvider + Send + Sync + 'static,
    ) -> Self {
        self.anisette_generator = Some(AnisetteDataGenerator::new(Arc::new(RwLock::new(
            anisette_provider,
        ))));
        self
    }

    /// Build the AppleAccount without logging in
    ///
    /// # Errors
    /// Returns an error if the reqwest client cannot be built
    pub async fn build(self) -> Result<AppleAccount, Report> {
        let debug = self.debug.unwrap_or(false);
        let anisette_generator = match self.anisette_generator {
            Some(generator) => generator,
            None => {
                let provider = RemoteV3AnisetteProvider::default()?;
                AnisetteDataGenerator::new(Arc::new(RwLock::new(provider)))
            }
        };

        AppleAccount::new(&self.email, anisette_generator, debug, self.proxy_url).await
    }

    /// Build the AppleAccount and log in
    ///
    /// # Arguments
    /// - `password`: The Apple ID password
    /// - `two_factor_callback`: A callback function that returns the two-factor authentication code
    /// # Errors
    /// Returns an error if the reqwest client cannot be built
    #[cfg(target_arch = "wasm32")]
    pub async fn login<C, Fut>(
        self,
        password: &str,
        two_factor_callback: C,
    ) -> Result<AppleAccount, Report>
    where
        C: Fn(TwoFactorCallbackParams) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<TwoFactorCallbackResponse, Report>>,
    {
        let mut account = self.build().await?;
        account.login(password, two_factor_callback).await?;
        Ok(account)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn login<C, Fut>(
        self,
        password: &str,
        two_factor_callback: C,
    ) -> Result<AppleAccount, Report>
    where
        C: Fn(TwoFactorCallbackParams) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = Result<TwoFactorCallbackResponse, Report>> + Send,
    {
        let mut account = self.build().await?;
        account.login(password, two_factor_callback).await?;
        Ok(account)
    }
}
