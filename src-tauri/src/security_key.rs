use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use authenticator::{
    Pin, StatusPinUv, StatusUpdate,
    authenticatorservice::{AuthenticatorService, SignArgs},
    ctap2::server::{
        AuthenticationExtensionsClientInputs, PublicKeyCredentialDescriptor, Transport,
        UserVerificationRequirement,
    },
    statecallback::StateCallback,
};
use base64::Engine as _;
use base64::prelude::{BASE64_STANDARD, BASE64_STANDARD_NO_PAD, BASE64_URL_SAFE_NO_PAD};
use hmac::Hmac;
use isideload::auth::apple_account::AppleAccount;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use srp::groups::G2048;
use tauri::{Emitter, Listener, Window};
use tracing::{debug, info, warn};

use crate::error::AppError;

const GSA_AUTH_URL: &str = "https://gsa.apple.com/auth";
const GSA_SECURITY_KEY_URL: &str = "https://gsa.apple.com/auth/verify/security/key";
const IDMSA_AUTH_URL: &str = "https://idmsa.apple.com/appleauth/auth";
/// widget key used by account.apple.com's sign-in widget.
const APPLE_WIDGET_KEY: &str = "af1139274f266b22b68c2a3e7ad932cb3c0bbe854e13a79af78dcc73136882c3";
const WEB_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";
const ASSERTION_TIMEOUT_MS: u64 = 120_000;
const PIN_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone)]
pub struct SecurityKeyChallenge {
    pub challenge: String,
    pub key_handles: Vec<Vec<u8>>,
    pub rp_id: String,
    pub key_names: Vec<String>,
    pub source: ChallengeSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeSource {
    Gsa,
    Idmsa,
}

#[derive(Debug, Clone)]
pub struct AssertionData {
    pub credential_id: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub user_handle: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SecurityKeyVerifyBody {
    challenge: String,
    client_data: String,
    signature_data: String,
    authenticator_data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_handle: Option<String>,
    #[serde(rename = "credentialID")]
    credential_id: String,
    rp_id: String,
}

pub async fn try_security_key_login(
    window: &Window,
    account: &mut AppleAccount,
    password: &str,
) -> Result<bool, AppError> {
    if account.spd.is_none() {
        debug!("No partial session available, skipping security key flow");
        return Ok(false);
    }

    let mut challenge: Option<SecurityKeyChallenge> = None;

    match fetch_challenge_gsa(account).await {
        Ok(Some(mut c)) => {
            c.source = ChallengeSource::Gsa;
            challenge = Some(c);
        }
        Ok(None) => debug!("No security key challenge found via GSA auth options"),
        Err(e) => warn!("Failed to fetch auth options from GSA: {e}"),
    }

    let mut idmsa_session: Option<IdmsaSession> = None;
    if challenge.is_none() {
        match IdmsaSession::start(&account.email, password).await {
            Ok(Some(session)) => match session.fetch_challenge().await {
                Ok(Some(mut c)) => {
                    c.source = ChallengeSource::Idmsa;
                    challenge = Some(c);
                    idmsa_session = Some(session);
                }
                Ok(None) => debug!("No security key challenge found via idmsa"),
                Err(e) => warn!("Failed to fetch auth options from idmsa: {e}"),
            },
            Ok(None) => debug!("idmsa session did not require second factor"),
            Err(e) => warn!("Failed to start idmsa session: {e}"),
        }
    }

    let Some(challenge) = challenge else {
        info!("Account does not expose a security key challenge");
        return Ok(false);
    };

    info!(
        "Security key required for this account (registered keys: {})",
        if challenge.key_names.is_empty() {
            "unknown".to_string()
        } else {
            challenge.key_names.join(", ")
        }
    );

    window
        .emit(
            "security-key-required",
            serde_json::json!({ "keyNames": challenge.key_names }),
        )
        .map_err(|e| AppError::Misc(format!("Failed to emit security-key-required event: {e}")))?;

    let result =
        perform_security_key_challenge(window, account, challenge, idmsa_session, password).await;
    let _ = window.emit("security-key-close", serde_json::json!({}));
    result
}

async fn perform_security_key_challenge(
    window: &Window,
    account: &mut AppleAccount,
    challenge: SecurityKeyChallenge,
    idmsa_session: Option<IdmsaSession>,
    password: &str,
) -> Result<bool, AppError> {
    match challenge.source {
        ChallengeSource::Gsa => {
            let (assertion, client_data_json) = run_assertion(window, &challenge).await?;
            let body = build_verify_body(&challenge, &client_data_json, &assertion);

            if submit_via_gsa(account, &body).await? {
                return Ok(true);
            }

            warn!("GSA rejected the security key assertion, retrying via the idmsa web flow");
            let _ = window.emit(
                "security-key-status",
                serde_json::json!({ "kind": "retry" }),
            );

            let session = IdmsaSession::start(&account.email, password)
                .await?
                .ok_or_else(|| {
                    AppError::Auth("idmsa did not require a second factor".to_string())
                })?;
            let fresh_challenge = session.fetch_challenge().await?.ok_or_else(|| {
                AppError::Auth("idmsa did not offer a security key challenge".to_string())
            })?;
            let (assertion, client_data_json) = run_assertion(window, &fresh_challenge).await?;
            let body = build_verify_body(&fresh_challenge, &client_data_json, &assertion);

            if session.submit_assertion(&body).await? {
                return Ok(true);
            }
            if submit_via_gsa(account, &body).await? {
                return Ok(true);
            }

            Err(AppError::Auth(
                "Apple rejected the security key verification. Please try again.".to_string(),
            ))
        }
        ChallengeSource::Idmsa => {
            let session = idmsa_session.ok_or_else(|| {
                AppError::Auth("idmsa session missing for idmsa challenge".to_string())
            })?;
            let (assertion, client_data_json) = run_assertion(window, &challenge).await?;
            let body = build_verify_body(&challenge, &client_data_json, &assertion);

            if session.submit_assertion(&body).await? {
                return Ok(true);
            }
            if submit_via_gsa(account, &body).await? {
                return Ok(true);
            }

            Err(AppError::Auth(
                "Apple rejected the security key verification. Please try again.".to_string(),
            ))
        }
    }
}

fn build_verify_body(
    challenge: &SecurityKeyChallenge,
    client_data_json: &str,
    assertion: &AssertionData,
) -> SecurityKeyVerifyBody {
    let body = SecurityKeyVerifyBody {
        challenge: challenge.challenge.clone(),
        client_data: BASE64_STANDARD.encode(client_data_json),
        signature_data: BASE64_STANDARD.encode(&assertion.signature),
        authenticator_data: BASE64_STANDARD.encode(&assertion.authenticator_data),
        user_handle: assertion
            .user_handle
            .as_ref()
            .map(|u| BASE64_STANDARD_NO_PAD.encode(u)),
        credential_id: BASE64_STANDARD_NO_PAD.encode(&assertion.credential_id),
        rp_id: challenge.rp_id.clone(),
    };
    debug!(
        "Security key assertion body: {}",
        serde_json::to_string(&body).unwrap_or_default()
    );
    body
}

async fn run_assertion(
    window: &Window,
    challenge: &SecurityKeyChallenge,
) -> Result<(AssertionData, String), AppError> {
    let client_data_json = build_client_data_json(&challenge.challenge);
    let client_data_hash: [u8; 32] = Sha256::digest(client_data_json.as_bytes()).into();

    let task_challenge = challenge.clone();
    let blocking_window = window.clone();
    let assertion = tokio::task::spawn_blocking(move || {
        ctap2_get_assertion(blocking_window, &task_challenge, client_data_hash)
    })
    .await
    .map_err(|e| AppError::Misc(format!("Security key task panicked: {e}")))??;

    Ok((assertion, client_data_json))
}

async fn build_2fa_headers(account: &mut AppleAccount) -> Result<HeaderMap, AppError> {
    let anisette_data = account
        .anisette_generator
        .get_anisette_data(account.grandslam_client.clone())
        .await
        .map_err(|e| AppError::Anisette(format!("Failed to get anisette data: {e}")))?;

    let mut headers = anisette_data
        .get_header_map()
        .map_err(|e| AppError::Anisette(format!("Failed to build anisette headers: {e}")))?;

    let spd = account
        .spd
        .as_ref()
        .ok_or_else(|| AppError::Auth("No session data available".to_string()))?;

    let get_str = |key: &str| -> Result<String, AppError> {
        spd.get(key)
            .and_then(|v| v.as_string())
            .map(|s| s.to_string())
            .ok_or_else(|| AppError::Auth(format!("Missing {key} in session data")))
    };
    let adsid = get_str("adsid")?;
    let token = get_str("GsIdmsToken")?;

    let identity = BASE64_STANDARD.encode(format!("{}:{}", adsid, token));
    headers.insert(
        "X-Apple-Identity-Token",
        HeaderValue::from_str(&identity)
            .map_err(|e| AppError::Auth(format!("Invalid identity token: {e}")))?,
    );
    headers.insert(
        "X-Apple-I-MD-RINFO",
        HeaderValue::from_str(&anisette_data.routing_info)
            .map_err(|e| AppError::Auth(format!("Invalid routing info: {e}")))?,
    );

    Ok(headers)
}

async fn fetch_challenge_gsa(
    account: &mut AppleAccount,
) -> Result<Option<SecurityKeyChallenge>, AppError> {
    let headers = build_2fa_headers(account).await?;

    let response = account
        .grandslam_client
        .get_sms(GSA_AUTH_URL)?
        .headers(headers)
        .send()
        .await
        .map_err(|e| AppError::Auth(format!("Failed to fetch GSA auth options: {e}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| AppError::Auth(format!("Failed to read GSA auth options: {e}")))?;
    debug!("GSA auth options response ({}): {}", status, text);

    if text.is_empty() {
        return Ok(None);
    }
    Ok(parse_challenge_json(&text))
}

async fn submit_via_gsa(
    account: &mut AppleAccount,
    body: &SecurityKeyVerifyBody,
) -> Result<bool, AppError> {
    let headers = build_2fa_headers(account).await?;

    let response = account
        .grandslam_client
        .post_sms(GSA_SECURITY_KEY_URL)?
        .headers(headers)
        .body(serde_json::to_string(body).map_err(|e| {
            AppError::Auth(format!("Failed to serialize security key request: {e}"))
        })?)
        .send()
        .await
        .map_err(|e| AppError::Auth(format!("Failed to submit security key assertion: {e}")))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    debug!("GSA security key verify response ({}): {}", status, text);

    Ok(status.is_success())
}

struct IdmsaSession {
    client: reqwest::Client,
    session_id: String,
    scnt: String,
}

impl IdmsaSession {
    async fn start(email: &str, password: &str) -> Result<Option<Self>, AppError> {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .build()
            .map_err(|e| AppError::Auth(format!("Failed to build HTTP client: {e}")))?;

        let base = |req: reqwest::RequestBuilder| {
            req.header("User-Agent", WEB_USER_AGENT)
                .header("X-Apple-Widget-Key", APPLE_WIDGET_KEY)
                .header("X-Apple-OAuth-Client-Id", APPLE_WIDGET_KEY)
                .header("X-Apple-OAuth-Client-Type", "firstPartyAuth")
                .header("X-Apple-OAuth-Redirect-URI", "https://account.apple.com")
                .header("X-Apple-OAuth-Response-Mode", "web_message")
                .header("X-Apple-OAuth-Response-Type", "code")
                .header("X-Requested-With", "XMLHttpRequest")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/javascript, */*; q=0.01")
                .header("Origin", "https://idmsa.apple.com")
                .header("Referer", "https://idmsa.apple.com/")
        };

        let response = base(client.post(format!("{IDMSA_AUTH_URL}/federate")))
            .body(serde_json::json!({ "accountName": email, "rememberMe": false }).to_string())
            .send()
            .await
            .map_err(|e| AppError::Auth(format!("idmsa federate failed: {e}")))?;

        let mut session = Self {
            session_id: extract_header(&response, "X-Apple-ID-Session-Id").unwrap_or_default(),
            scnt: extract_header(&response, "scnt").unwrap_or_default(),
            client: client.clone(),
        };
        debug!(
            "idmsa federate: {} (session id present: {})",
            response.status(),
            !session.session_id.is_empty()
        );

        let srp_client = srp::Client::<G2048, Sha256>::new_with_options(false);
        let a: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let a_pub = srp_client.compute_public_ephemeral(&a);

        let response = base(client.post(format!("{IDMSA_AUTH_URL}/signin/init")))
            .headers(session.auth_headers())
            .body(
                serde_json::json!({
                    "a": BASE64_STANDARD.encode(&a_pub),
                    "accountName": email,
                    "protocols": ["s2k", "s2k_fo"],
                })
                .to_string(),
            )
            .send()
            .await
            .map_err(|e| AppError::Auth(format!("idmsa signin/init failed: {e}")))?;

        session.update_from_response(&response);
        let status = response.status();
        let init_text = response
            .text()
            .await
            .map_err(|e| AppError::Auth(format!("Failed to read idmsa signin/init: {e}")))?;
        debug!("idmsa signin/init ({}): {}", status, init_text);
        if !status.is_success() {
            return Err(AppError::Auth(format!(
                "idmsa signin/init failed with status {status}"
            )));
        }
        let init: serde_json::Value = serde_json::from_str(&init_text)
            .map_err(|e| AppError::Auth(format!("Failed to parse idmsa signin/init: {e}")))?;

        let iteration = init
            .get("iteration")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| AppError::Auth("Missing iteration in signin/init".to_string()))?;
        let salt = BASE64_STANDARD
            .decode(
                init.get("salt")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AppError::Auth("Missing salt in signin/init".to_string()))?,
            )
            .map_err(|e| AppError::Auth(format!("Invalid salt in signin/init: {e}")))?;
        let protocol = init
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("s2k")
            .to_string();
        let b_pub = BASE64_STANDARD
            .decode(
                init.get("b")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AppError::Auth("Missing b in signin/init".to_string()))?,
            )
            .map_err(|e| AppError::Auth(format!("Invalid b in signin/init: {e}")))?;
        let c = init
            .get("c")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::Auth("Missing c in signin/init".to_string()))?
            .to_string();

        let hashed_password = Sha256::digest(password.as_bytes());
        let password_hash: Vec<u8> = if protocol == "s2k_fo" {
            hex::encode(hashed_password).into_bytes()
        } else {
            hashed_password.to_vec()
        };
        let mut password_buf = [0u8; 32];
        pbkdf2::pbkdf2::<Hmac<Sha256>>(&password_hash, &salt, iteration as u32, &mut password_buf)
            .map_err(|e| AppError::Auth(format!("Failed to derive password key: {e}")))?;

        let verifier = srp_client
            .process_reply(&a, email.as_bytes(), &password_buf, &salt, &b_pub)
            .map_err(|e| AppError::Auth(format!("SRP handshake failed: {e}")))?;

        let m1 = verifier.proof().to_vec();
        let m2: [u8; 32] = Sha256::new()
            .chain_update(&a_pub)
            .chain_update(&m1)
            .chain_update(verifier.key())
            .finalize()
            .into();

        let response = base(client.post(format!("{IDMSA_AUTH_URL}/signin/complete")))
            .headers(session.auth_headers())
            .body(
                serde_json::json!({
                    "accountName": email,
                    "rememberMe": false,
                    "m1": BASE64_STANDARD.encode(&m1),
                    "c": c,
                    "m2": BASE64_STANDARD.encode(m2),
                })
                .to_string(),
            )
            .send()
            .await
            .map_err(|e| AppError::Auth(format!("idmsa signin/complete failed: {e}")))?;

        session.update_from_response(&response);
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AppError::Auth(format!("Failed to read idmsa signin/complete: {e}")))?;
        debug!("idmsa signin/complete ({}): {}", status, text);

        if status.as_u16() == 409 {
            Ok(Some(session))
        } else if status.is_success() {
            Ok(None)
        } else {
            Err(AppError::Auth(format!(
                "idmsa sign in failed with status {status}: {text}"
            )))
        }
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(&self.session_id) {
            headers.insert("X-Apple-ID-Session-Id", value);
        }
        if let Ok(value) = HeaderValue::from_str(&self.scnt) {
            headers.insert("scnt", value);
        }
        headers
    }

    fn update_from_response(&mut self, response: &reqwest::Response) {
        if let Some(value) = extract_header(response, "X-Apple-ID-Session-Id") {
            self.session_id = value;
        }
        if let Some(value) = extract_header(response, "scnt") {
            self.scnt = value;
        }
    }

    async fn fetch_challenge(&self) -> Result<Option<SecurityKeyChallenge>, AppError> {
        let response = self
            .client
            .get(IDMSA_AUTH_URL)
            .header("User-Agent", WEB_USER_AGENT)
            .header("X-Apple-Widget-Key", APPLE_WIDGET_KEY)
            .header("X-Apple-OAuth-Client-Id", APPLE_WIDGET_KEY)
            .header("X-Apple-OAuth-Client-Type", "firstPartyAuth")
            .header("X-Apple-OAuth-Redirect-URI", "https://account.apple.com")
            .header("X-Apple-OAuth-Response-Mode", "web_message")
            .header("X-Apple-OAuth-Response-Type", "code")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Accept", "application/json, text/html, */*")
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Auth(format!("Failed to fetch idmsa auth options: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AppError::Auth(format!("Failed to read idmsa auth options: {e}")))?;
        debug!("idmsa auth options response ({}): {}", status, text);

        if text.trim_start().starts_with('<') {
            Ok(parse_challenge_html(&text))
        } else {
            Ok(parse_challenge_json(&text))
        }
    }

    async fn submit_assertion(&self, body: &SecurityKeyVerifyBody) -> Result<bool, AppError> {
        let response = self
            .client
            .post(format!("{IDMSA_AUTH_URL}/verify/security/key"))
            .header("User-Agent", WEB_USER_AGENT)
            .header("X-Apple-Widget-Key", APPLE_WIDGET_KEY)
            .header("X-Apple-App-Id", APPLE_WIDGET_KEY)
            .header("X-Apple-OAuth-Client-Id", APPLE_WIDGET_KEY)
            .header("X-Apple-OAuth-Client-Type", "firstPartyAuth")
            .header("X-Apple-OAuth-Redirect-URI", "https://account.apple.com")
            .header("X-Apple-OAuth-Response-Mode", "web_message")
            .header("X-Apple-OAuth-Response-Type", "code")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/plain, */*")
            .header("Origin", "https://idmsa.apple.com")
            .header("Referer", "https://idmsa.apple.com/")
            .headers(self.auth_headers())
            .body(serde_json::to_string(body).map_err(|e| {
                AppError::Auth(format!("Failed to serialize security key request: {e}"))
            })?)
            .send()
            .await
            .map_err(|e| AppError::Auth(format!("Failed to submit security key assertion: {e}")))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        debug!("idmsa security key verify response ({}): {}", status, text);

        Ok(status.is_success())
    }
}

fn extract_header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn build_client_data_json(challenge_b64: &str) -> String {
    let challenge_b64url = match b64_flex_decode(challenge_b64) {
        Some(decoded) => BASE64_URL_SAFE_NO_PAD.encode(decoded),
        None => challenge_b64.to_string(),
    };
    format!(
        r#"{{"type":"webauthn.get","challenge":"{challenge_b64url}","origin":"https://idmsa.apple.com","crossOrigin":true,"topOrigin":"https://account.apple.com"}}"#
    )
}

fn b64_flex_decode(value: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    BASE64_STANDARD_NO_PAD
        .decode(value)
        .or_else(|_| BASE64_STANDARD.decode(value))
        .ok()
}

fn find_json_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(found);
            }
            map.values().find_map(|v| find_json_key(v, key))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|v| find_json_key(v, key)),
        _ => None,
    }
}

fn parse_challenge_object(
    object: &serde_json::Value,
    document: &serde_json::Value,
) -> Option<SecurityKeyChallenge> {
    let challenge = object.get("challenge")?.as_str()?.to_string();
    let rp_id = object
        .get("rpId")
        .and_then(|v| v.as_str())
        .unwrap_or("apple.com")
        .to_string();
    let key_handles: Vec<Vec<u8>> =
        object
            .get("keyHandles")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(b64_flex_decode)
                    .collect()
            })?;
    if key_handles.is_empty() {
        return None;
    }
    let key_names = find_json_key(document, "keyNames")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    Some(SecurityKeyChallenge {
        challenge,
        key_handles,
        rp_id,
        key_names,
        source: ChallengeSource::Gsa,
    })
}

fn parse_challenge_json(text: &str) -> Option<SecurityKeyChallenge> {
    let document: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = find_json_key(&document, "fsaChallenge")?;
    parse_challenge_object(object, &document)
}

fn parse_challenge_html(html: &str) -> Option<SecurityKeyChallenge> {
    let marker = r#"<script type="application/json" class="boot_args">"#;
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find("</script>")? + start;
    let document: serde_json::Value = serde_json::from_str(html[start..end].trim()).ok()?;
    let object = find_json_key(&document, "fsaChallenge")?;
    parse_challenge_object(object, &document)
}

fn ctap2_get_assertion(
    window: Window,
    challenge: &SecurityKeyChallenge,
    client_data_hash: [u8; 32],
) -> Result<AssertionData, AppError> {
    let manager = AuthenticatorService::new()
        .map_err(|e| AppError::Auth(format!("Failed to initialize security key service: {e:?}")))?;
    let mut manager = manager;
    manager.add_u2f_usb_hid_platform_transports();
    let manager = Arc::new(Mutex::new(manager));

    let (status_tx, status_rx) = mpsc::channel::<StatusUpdate>();

    {
        let window = window.clone();
        let manager = manager.clone();
        std::thread::spawn(move || {
            loop {
                match status_rx.recv() {
                    Ok(StatusUpdate::SelectDeviceNotice) => {
                        debug!("Multiple security keys detected, touch the one to use");
                        let _ = window.emit(
                            "security-key-status",
                            serde_json::json!({ "kind": "touch", "message": "select" }),
                        );
                    }
                    Ok(StatusUpdate::PresenceRequired) => {
                        debug!("Waiting for security key touch");
                        let _ = window.emit(
                            "security-key-status",
                            serde_json::json!({ "kind": "touch", "message": "touch" }),
                        );
                    }
                    Ok(StatusUpdate::PinUvError(StatusPinUv::PinRequired(sender))) => {
                        match prompt_for_pin(&window, None, &manager) {
                            Some(pin) => {
                                let _ = sender.send(Pin::new(&pin));
                            }
                            None => break,
                        }
                    }
                    Ok(StatusUpdate::PinUvError(StatusPinUv::InvalidPin(sender, attempts))) => {
                        warn!("Invalid security key PIN entered");
                        let _ = window.emit(
                            "security-key-status",
                            serde_json::json!({ "kind": "invalidPin", "attempts": attempts }),
                        );
                        match prompt_for_pin(&window, attempts, &manager) {
                            Some(pin) => {
                                let _ = sender.send(Pin::new(&pin));
                            }
                            None => break,
                        }
                    }
                    Ok(StatusUpdate::PinUvError(StatusPinUv::PinAuthBlocked)) => {
                        warn!("Security key temporarily blocked, needs replug");
                        let _ = window.emit(
                            "security-key-status",
                            serde_json::json!({ "kind": "error", "message": "pinAuthBlocked" }),
                        );
                        break;
                    }
                    Ok(StatusUpdate::PinUvError(StatusPinUv::PinBlocked)) => {
                        warn!("Security key blocked, needs reset");
                        let _ = window.emit(
                            "security-key-status",
                            serde_json::json!({ "kind": "error", "message": "pinBlocked" }),
                        );
                        break;
                    }
                    Ok(StatusUpdate::PinUvError(e)) => {
                        warn!("Unexpected security key status: {e:?}");
                        let _ = window.emit(
                            "security-key-status",
                            serde_json::json!({ "kind": "error", "message": format!("{e:?}") }),
                        );
                        break;
                    }
                    Ok(other) => {
                        debug!("Security key status: {other:?}");
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let allow_list: Vec<PublicKeyCredentialDescriptor> = challenge
        .key_handles
        .iter()
        .map(|id| PublicKeyCredentialDescriptor {
            id: id.clone(),
            transports: vec![Transport::USB],
        })
        .collect();

    let args = SignArgs {
        client_data_hash,
        origin: "https://idmsa.apple.com".to_string(),
        relying_party_id: challenge.rp_id.clone(),
        allow_list,
        user_verification_req: UserVerificationRequirement::Required,
        user_presence_req: true,
        extensions: AuthenticationExtensionsClientInputs::default(),
        pin: None,
        use_ctap1_fallback: false,
    };

    let (result_tx, result_rx) = mpsc::channel();
    let callback = StateCallback::new(Box::new(move |rv| {
        let _ = result_tx.send(rv);
    }));

    if let Err(e) = manager
        .lock()
        .unwrap()
        .sign(ASSERTION_TIMEOUT_MS, args, status_tx, callback)
    {
        return Err(AppError::Auth(format!(
            "Failed to start security key assertion: {e:?}"
        )));
    }

    let result = result_rx
        .recv_timeout(Duration::from_millis(ASSERTION_TIMEOUT_MS + 15_000))
        .map_err(|_| {
            AppError::Auth(
                "Timed out waiting for the security key. Make sure it is plugged in and try again."
                    .to_string(),
            )
        })?;

    match result {
        Ok(sign_result) => {
            let assertion = sign_result.assertion;
            let credential_id = assertion
                .credentials
                .map(|c| c.id)
                .ok_or_else(|| AppError::Auth("Security key returned no credential".to_string()))?;
            let user_handle = assertion.user.map(|u| u.id);
            info!("Security key assertion obtained");
            Ok(AssertionData {
                credential_id,
                authenticator_data: assertion.auth_data.to_vec(),
                signature: assertion.signature,
                user_handle,
            })
        }
        Err(e) => Err(AppError::Auth(format!(
            "Security key assertion failed: {e:?}"
        ))),
    }
}

fn prompt_for_pin(
    window: &Window,
    attempts: Option<u8>,
    manager: &Arc<Mutex<AuthenticatorService>>,
) -> Option<String> {
    let _ = window.emit(
        "security-key-status",
        serde_json::json!({ "kind": "pinRequired", "attempts": attempts }),
    );

    let (tx, rx) = mpsc::channel::<Option<String>>();
    let handler_id = window.listen("security-key-response", move |event| {
        #[derive(Deserialize)]
        struct Response {
            pin: Option<String>,
        }
        let response: Result<Response, _> = serde_json::from_str(event.payload());
        match response {
            Ok(Response { pin: Some(pin) }) => {
                let _ = tx.send(Some(pin));
            }
            _ => {
                let _ = tx.send(None);
            }
        }
    });

    let result = rx.recv_timeout(Duration::from_secs(PIN_TIMEOUT_SECS)).ok();
    window.unlisten(handler_id);

    match result {
        Some(Some(pin)) => Some(pin),
        Some(None) => {
            if let Ok(mut manager) = manager.lock() {
                let _ = manager.cancel();
            }
            None
        }
        None => {
            if let Ok(mut manager) = manager.lock() {
                let _ = manager.cancel();
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_body_field_names_match_web_client() {
        let body = SecurityKeyVerifyBody {
            challenge: "a+b/c".to_string(),
            client_data: "e30".to_string(),
            signature_data: "MEUC".to_string(),
            authenticator_data: "ImXL".to_string(),
            user_handle: Some("Zlln".to_string()),
            credential_id: "GJFc".to_string(),
            rp_id: "apple.com".to_string(),
        };
        let json = serde_json::to_value(&body).unwrap();
        let object = json.as_object().unwrap();
        for key in [
            "challenge",
            "clientData",
            "signatureData",
            "authenticatorData",
            "userHandle",
            "credentialID",
            "rpId",
        ] {
            assert!(object.contains_key(key), "missing key {key}");
        }
        assert_eq!(object.len(), 7);
    }
}
