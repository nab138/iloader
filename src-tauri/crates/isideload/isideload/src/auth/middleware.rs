use reqwest_middleware::{Middleware, Next};

pub struct WasmProxyMiddleware {
    proxy_url: String,
}

impl WasmProxyMiddleware {
    pub fn new(proxy_url: String) -> Self {
        Self { proxy_url }
    }
}

// You might be wondering, why does this cfg_attr check the target_arch AND the feature when all the others just check the feature?
// For some reason, I could not get rust analyzer to output errors when the arch was set to wasm32-unknown-unknown
// In an effort to make my life not miserable, I wanted isideload to compile as x86 even with the wasm feature enabled
// This one trait however didn't agree with that, so I added the additional check
// The arch check caused problems in other places, so everywhere else just gets the feature check
#[cfg_attr(all(target_arch = "wasm32", feature = "wasm"), async_trait::async_trait(?Send))]
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "wasm")),
    async_trait::async_trait
)]
impl Middleware for WasmProxyMiddleware {
    async fn handle(
        &self,
        mut req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let original = req.url().to_string();
        let proxied = format!("{}{}", self.proxy_url, urlencoding::encode(&original));
        *req.url_mut() = proxied.parse().unwrap();
        next.run(req, extensions).await
    }
}
