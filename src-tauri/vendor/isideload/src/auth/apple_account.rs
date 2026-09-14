use std::{future::Future, sync::Arc};

use crate::{
    SideloadError,
    anisette::{AnisetteData, AnisetteDataGenerator},
    auth::{
        builder::AppleAccountBuilder,
        grandslam::{GrandSlam, GrandSlamErrorChecker},
    },
    util::plist::{PlistDataExtract, SensitivePlistAttachment},
};
use aes::{
    Aes256,
    cipher::{block_padding::Pkcs7, consts::U16},
};
use aes_gcm::{AeadInOut, AesGcm, KeyInit, Nonce};
use base64::{Engine, prelude::BASE64_STANDARD};
use cbc::cipher::{BlockModeDecrypt, KeyIvInit};
use hmac::{Hmac, Mac};
use plist::Dictionary;
use plist_macro::plist;
use reqwest::header::{HeaderMap, HeaderValue};
use rootcause::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use srp::{ClientVerifier, groups::G2048};
use tracing::{debug, info, warn};

pub struct AppleAccount {
    pub email: String,
    pub spd: Option<plist::Dictionary>,
    pub anisette_generator: AnisetteDataGenerator,
    pub grandslam_client: Arc<GrandSlam>,
    pub trusted_phone_numbers: Option<Vec<TrustedNumber>>,
    login_state: LoginState,
    debug: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LoginState {
    LoggedIn,
    NeedsDevice2FA,
    NeedsDevice2FAVerification,
    NeedsSMS2FA(u32),
    NeedsSMS2FAVerification(u32),
    NeedsUnknown2FA,
    NeedsExtraStep(String),
    NeedsLogin,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TrustedNumber {
    pub number_with_dial_code: String,
    pub last_two_digits: String,
    pub push_mode: String,
    pub id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwoFactorCallbackParams {
    pub last_error: Option<String>,
    // If this is true, we don't know what's going to work, so present the user with all the options and let them choose
    pub unknown: bool,
    pub sms: bool,
    pub numbers: Vec<TrustedNumber>,
    pub selected_number_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TwoFactorCallbackResponse {
    SubmitCode(String),
    SendSms(u32),
    SendToDevices,
    ResendCode,
    Abort,
}

#[derive(Debug, Clone)]
pub struct SMSTwoFactorError {
    pub code: String,
    pub title: String,
    pub message: String,
}

#[derive(Debug)]
enum SmsSendOutcome {
    Sent,
    ActiveChallenge,
    ServiceError(SMSTwoFactorError),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SmsChallengeResponse {
    mode: String,
    #[serde(rename = "type")]
    challenge_type: String,
    authentication_type: String,
    trusted_phone_numbers: Vec<TrustedNumber>,
    trusted_phone_number: TrustedNumber,
    security_code: SmsSecurityCode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SmsSecurityCode {
    length: u8,
    too_many_codes_sent: bool,
    too_many_codes_validated: bool,
    security_code_locked: bool,
    security_code_cooldown: bool,
}

impl AppleAccount {
    /// Create a new AppleAccountBuilder with the given email
    ///
    /// # Arguments
    /// - `email`: The Apple ID email address
    pub fn builder(email: &str) -> AppleAccountBuilder {
        AppleAccountBuilder::new(email)
    }

    /// Build the apple account with the given email
    ///
    /// Reccomended to use the AppleAccountBuilder instead
    /// # Arguments
    /// - `email`: The Apple ID email address
    /// - `anisette_provider`: The anisette provider to use
    /// - `debug`: DANGER, If true, accept invalid certificates and enable verbose connection
    pub async fn new(
        email: &str,
        anisette_generator: AnisetteDataGenerator,
        debug: bool,
        proxy_url: Option<String>,
    ) -> Result<Self, Report> {
        if debug {
            warn!("Debug mode enabled: this is a security risk!");
        }

        let client_info = anisette_generator
            .get_client_info()
            .await
            .context("Failed to get anisette client info")?;

        let grandslam_client = GrandSlam::new(client_info, debug, proxy_url.clone()).await?;

        Ok(AppleAccount {
            email: email.to_string(),
            spd: None,
            anisette_generator,
            grandslam_client: Arc::new(grandslam_client),
            debug,
            login_state: LoginState::NeedsLogin,
            trusted_phone_numbers: None,
            last_error: None,
        })
    }

    /// Log in to the Apple ID account
    /// # Arguments
    /// - `password`: The Apple ID password
    /// - `two_factor_callback`: A callback function that returns the two-factor authentication code
    /// # Errors
    /// Returns an error if the login fails
    #[cfg(target_arch = "wasm32")]
    pub async fn login<C, Fut>(
        &mut self,
        password: &str,
        two_factor_callback: C,
    ) -> Result<(), Report>
    where
        C: Fn(TwoFactorCallbackParams) -> Fut + Send + Sync,
        Fut: Future<Output = Result<TwoFactorCallbackResponse, Report>>,
    {
        self.login_impl(password, two_factor_callback).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn login<C, Fut>(
        &mut self,
        password: &str,
        two_factor_callback: C,
    ) -> Result<(), Report>
    where
        C: Fn(TwoFactorCallbackParams) -> Fut + Send + Sync,
        Fut: Future<Output = Result<TwoFactorCallbackResponse, Report>> + Send,
    {
        self.login_impl(password, two_factor_callback).await
    }

    async fn login_impl<C, Fut>(
        &mut self,
        password: &str,
        two_factor_callback: C,
    ) -> Result<(), Report>
    where
        C: Fn(TwoFactorCallbackParams) -> Fut + Send + Sync,
        Fut: Future<Output = Result<TwoFactorCallbackResponse, Report>>,
    {
        info!("Logging in to Apple ID: {}", censor_email(&self.email));
        if self.debug {
            warn!("Debug mode enabled: this is a security risk!");
        }

        self.login_state = self
            .login_inner(password)
            .await
            .context("Failed to log in to Apple ID")?;

        debug!("Initial login successful");

        let mut attempts = 0;

        loop {
            attempts += 1;
            if attempts > 15 {
                bail!(
                    "Couldn't login after 15 attempts, aborting (current state: {:?})",
                    self.login_state
                );
            }
            match self.login_state.clone() {
                LoginState::LoggedIn => {
                    info!("Successfully logged in to Apple ID");
                    return Ok(());
                }
                LoginState::NeedsDevice2FA => {
                    if self.trusted_phone_numbers.is_none() {
                        self.trusted_phone_numbers = Some(self.get_trusted_numbers().await?);
                    }
                    self.send_trusted_device_2fa()
                        .await
                        .context("Failed to complete trusted device 2FA")?;
                    self.login_state = LoginState::NeedsDevice2FAVerification;
                }
                LoginState::NeedsDevice2FAVerification => {
                    let response = two_factor_callback(TwoFactorCallbackParams {
                        last_error: self.last_error.clone(),
                        unknown: false,
                        sms: false,
                        numbers: self.trusted_phone_numbers.clone().unwrap_or_default(),
                        selected_number_id: None,
                    })
                    .await?;
                    self.last_error = None;
                    match response {
                        TwoFactorCallbackResponse::SubmitCode(code) => {
                            self.login_state = self
                                .verify_trusted_device_2fa(code)
                                .await
                                .context("Failed to verify trusted device 2FA")?;
                        }
                        TwoFactorCallbackResponse::SendSms(selected_number_id) => {
                            self.login_state = self.select_number(selected_number_id)?;
                        }
                        TwoFactorCallbackResponse::SendToDevices
                        | TwoFactorCallbackResponse::ResendCode => {
                            self.login_state = LoginState::NeedsDevice2FA;
                        }
                        TwoFactorCallbackResponse::Abort => {
                            bail!("No 2FA code provided, aborting")
                        }
                    }
                }
                LoginState::NeedsSMS2FA(id) => {
                    if self.trusted_phone_numbers.is_none() {
                        self.trusted_phone_numbers = Some(self.get_trusted_numbers().await?);
                    }
                    info!("SMS 2FA required");
                    self.login_state = self
                        .send_sms_2fa(id)
                        .await
                        .context("Failed to complete SMS 2FA")?;
                }
                LoginState::NeedsSMS2FAVerification(id) => {
                    let response = two_factor_callback(TwoFactorCallbackParams {
                        unknown: false,
                        last_error: self.last_error.clone(),
                        sms: true,
                        numbers: self.trusted_phone_numbers.clone().unwrap_or_default(),
                        selected_number_id: Some(id),
                    })
                    .await?;
                    self.last_error = None;
                    match response {
                        TwoFactorCallbackResponse::SubmitCode(code) => {
                            self.login_state = self
                                .verify_sms_2fa(code, id)
                                .await
                                .context("Failed to verify trusted device 2FA")?;
                        }
                        TwoFactorCallbackResponse::SendSms(selected_number_id) => {
                            self.login_state = self.select_number(selected_number_id)?;
                        }
                        TwoFactorCallbackResponse::ResendCode => {
                            self.login_state = LoginState::NeedsSMS2FA(id);
                        }
                        TwoFactorCallbackResponse::SendToDevices => {
                            self.login_state = LoginState::NeedsDevice2FA;
                        }
                        TwoFactorCallbackResponse::Abort => {
                            bail!("No 2FA code provided, aborting")
                        }
                    }
                }
                LoginState::NeedsExtraStep(s) => {
                    info!("Additional authentication step required: {}", s);
                    if self.get_pet().is_err() {
                        bail!("Additional authentication required: {}", s);
                    }
                    self.login_state = LoginState::LoggedIn;
                }
                LoginState::NeedsLogin => {
                    debug!("Logging in again...");
                    self.login_state = self
                        .login_inner(password)
                        .await
                        .context("Failed to login again")?;
                }
                LoginState::NeedsUnknown2FA => {
                    info!(
                        "The most recently attempted 2FA Method failed, please try a different method."
                    );
                    let response = two_factor_callback(TwoFactorCallbackParams {
                        unknown: true,
                        last_error: self.last_error.clone(),
                        sms: false,
                        numbers: self.trusted_phone_numbers.clone().unwrap_or_default(),
                        selected_number_id: None,
                    })
                    .await?;
                    self.last_error = None;
                    match response {
                        TwoFactorCallbackResponse::SubmitCode(_) => {
                            bail!("Cannot submit code without knowing which method to use");
                        }
                        TwoFactorCallbackResponse::SendSms(selected_number_id) => {
                            self.login_state = self.select_number(selected_number_id)?;
                        }
                        TwoFactorCallbackResponse::SendToDevices => {
                            self.login_state = LoginState::NeedsDevice2FA;
                        }
                        TwoFactorCallbackResponse::ResendCode => {
                            bail!("Cannot resend code without knowing which method to use");
                        }
                        TwoFactorCallbackResponse::Abort => {
                            bail!("No 2FA method selected, aborting");
                        }
                    }
                }
            }
        }
    }

    /// Get the user's first and last name associated with the Apple ID
    pub fn get_name(&self) -> Result<(String, String), Report> {
        let spd = self
            .spd
            .as_ref()
            .ok_or_else(|| report!("SPD not available, cannot get name"))?;

        Ok((spd.get_string("fn")?, spd.get_string("ln")?))
    }

    fn get_pet(&self) -> Result<String, Report> {
        let spd = self
            .spd
            .as_ref()
            .ok_or_else(|| report!("SPD not available, cannot get pet"))?;

        let pet = spd
            .get_dict("t")?
            .get_dict("com.apple.gs.idms.pet")?
            .get_string("token")?;

        Ok(pet)
    }

    async fn send_trusted_device_2fa(&mut self) -> Result<(), Report> {
        debug!("Trusted device 2FA required");

        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for 2FA")?;

        let request_code_url = self
            .grandslam_client
            .get_url("trustedDeviceSecondaryAuth")?;

        self.grandslam_client
            .get(&request_code_url)?
            .headers(self.build_2fa_headers(&anisette_data).await?)
            .send()
            .await
            .context("Failed to request trusted device 2fa")?
            .error_for_status()
            .context("Trusted device 2FA request failed")?;

        info!("Trusted device 2FA request sent");

        Ok(())
    }

    async fn verify_trusted_device_2fa(&mut self, code: String) -> Result<LoginState, Report> {
        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for 2FA")?;

        let submit_code_url = self.grandslam_client.get_url("validateCode")?;

        let res = self
            .grandslam_client
            .get(&submit_code_url)?
            .headers(self.build_2fa_headers(&anisette_data).await?)
            .header("security-code", code)
            .send()
            .await
            .context("Failed to submit trusted device 2fa code")?
            .error_for_status()
            .context("Trusted device 2FA code submission failed")?
            .text()
            .await
            .context("Failed to read trusted device 2FA response text")?;

        let plist: Dictionary = plist::from_bytes(res.as_bytes())
            .context("Failed to parse trusted device response plist")
            .attach_with(|| res.clone())?;
        let res = plist
            .check_grandslam_error()
            .context("Trusted device 2FA rejected");
        if let Err(ref report) = res {
            for cause in report.iter_reports() {
                if let Some(err) = cause.downcast_current_context::<SideloadError>() {
                    match err {
                        &SideloadError::AuthWithMessage(code, ref message) => match code {
                            // Incorrect Verification Code, let the user try again
                            -21669 => {
                                warn!("{} - {}", code, message);
                                self.last_error = format!("{} - {}", code, message).into();

                                return Ok(LoginState::NeedsDevice2FAVerification);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
        }
        res?;

        debug!("Trusted device 2FA completed, need to login again");

        Ok(LoginState::NeedsLogin)
    }

    async fn send_sms_2fa(&mut self, id: u32) -> Result<LoginState, Report> {
        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for 2FA")?;

        //let request_code_url = self.grandslam_client.get_url("secondaryAuth")?;

        // self.grandslam_client
        //     .get_sms(&request_code_url)?
        //     .headers(self.build_2fa_headers(&anisette_data).await?)
        //     .send()
        //     .await
        //     .context("Failed to request SMS 2FA")?
        //     .error_for_status()
        //     .context("SMS 2FA request failed")?;

        let send_body = serde_json::json!({
            "phoneNumber": {
                "id": id
            },
            "mode": "sms"
        });

        let res = self
            .grandslam_client
            .put_sms("https://gsa.apple.com/auth/verify/phone")?
            .headers(self.build_2fa_headers(&anisette_data).await?)
            .body(send_body.to_string())
            .send()
            .await
            .context("Failed to request SMS 2FA")?;

        let status = res.status();
        let text = if status.is_success() {
            String::new()
        } else {
            res.text()
                .await
                .context("Failed to read SMS 2FA response text")?
        };

        match Self::classify_sms_send_response(status.as_u16(), &text, id)? {
            SmsSendOutcome::Sent => {
                info!("SMS 2FA request sent");
            }
            SmsSendOutcome::ActiveChallenge => {
                info!("SMS 2FA challenge already active, proceeding to verification");
            }
            SmsSendOutcome::ServiceError(error) => {
                if error.code == "-28248" {
                    warn!("{} - {}", error.title, error.message);
                    self.last_error = format!("{} - {}", error.title, error.message).into();
                    return Ok(LoginState::NeedsUnknown2FA);
                }

                if matches!(error.code.as_str(), "-22979" | "-22981") {
                    // Apple refused to send a new SMS, but the last code sent is
                    // still valid: keep the selected number and let the user
                    // enter it without triggering another send.
                    warn!("{} - {}", error.title, error.message);
                    self.last_error = format!("{} - {}", error.title, error.message).into();
                    return Ok(LoginState::NeedsSMS2FAVerification(id));
                }

                bail!(
                    "SMS 2FA request failed (code {}): {} - {}",
                    error.code,
                    error.title,
                    error.message
                );
            }
        }

        Ok(LoginState::NeedsSMS2FAVerification(id))
    }

    async fn verify_sms_2fa(&mut self, code: String, id: u32) -> Result<LoginState, Report> {
        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for 2FA")?;

        let body = serde_json::json!({
            "securityCode": {
                "code": code
            },
            "phoneNumber": {
                "id": id
            },
            "mode": "sms"
        });

        let res = self
            .grandslam_client
            .post_sms("https://gsa.apple.com/auth/verify/phone/securitycode")?
            .headers(self.build_2fa_headers(&anisette_data).await?)
            .body(body.to_string())
            .send()
            .await
            .context("Failed to submit SMS 2FA code")?;

        let status = res.status();
        let text = res
            .text()
            .await
            .context("Failed to read SMS 2FA error response text")?;
        if !status.is_success() {
            // try to parse as json, if it fails, just bail with the text
            let error = Self::parse_sms_error(text, status.as_u16())?;

            if error.code == "-21669" {
                // Incorrect Verification Code, let the user try again
                warn!("{} - {}", error.title, error.message);
                self.last_error = format!("{} - {}", error.title, error.message).into();
                return Ok(LoginState::NeedsSMS2FAVerification(id));
            }

            bail!(
                "SMS 2FA code submission failed (code {}): {} - {}",
                error.code,
                error.title,
                error.message
            );
        };

        debug!("SMS 2FA completed, need to login again");
        Ok(LoginState::NeedsLogin)
    }

    fn classify_sms_send_response(
        status: u16,
        text: &str,
        requested_number_id: u32,
    ) -> Result<SmsSendOutcome, Report> {
        if (200..300).contains(&status) {
            return Ok(SmsSendOutcome::Sent);
        }

        if let Some(error) = Self::parse_sms_service_error(text) {
            return Ok(SmsSendOutcome::ServiceError(error));
        }

        if status == 412
            && let Ok(challenge) = serde_json::from_str::<SmsChallengeResponse>(text)
            && challenge.mode == "sms"
            && challenge.challenge_type == "verification"
            && challenge.authentication_type == "hsa2"
            && challenge.trusted_phone_number.id == requested_number_id
            && challenge
                .trusted_phone_numbers
                .iter()
                .any(|number| number.id == requested_number_id)
            && challenge.security_code.length == 6
            && !challenge.security_code.too_many_codes_sent
            && !challenge.security_code.too_many_codes_validated
            && !challenge.security_code.security_code_locked
            && !challenge.security_code.security_code_cooldown
        {
            return Ok(SmsSendOutcome::ActiveChallenge);
        }

        bail!(
            "SMS 2FA request failed with http status {}: {}",
            status,
            text
        );
    }

    fn parse_sms_service_error(text: &str) -> Option<SMSTwoFactorError> {
        let json = serde_json::from_str::<serde_json::Value>(text).ok()?;
        let first_error = json.get("serviceErrors")?.as_array()?.first()?;
        let code = first_error
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("unknown");
        let title = first_error
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("No title provided");
        let message = first_error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("No message provided");

        Some(SMSTwoFactorError {
            code: code.to_string(),
            title: title.to_string(),
            message: message.to_string(),
        })
    }

    fn parse_sms_error(text: String, status: u16) -> Result<SMSTwoFactorError, Report> {
        Self::parse_sms_service_error(&text).ok_or_else(|| {
            report!(
                "SMS 2FA code submission failed with http status {}: {}",
                status,
                text
            )
        })
    }

    async fn get_trusted_numbers(&mut self) -> Result<Vec<TrustedNumber>, Report> {
        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for 2FA")?;

        let res = self
            .grandslam_client
            .get_sms("https://gsa.apple.com/auth")?
            .headers(self.build_2fa_headers(&anisette_data).await?)
            .send()
            .await?;

        let status = res.status().as_u16();
        let text = res
            .text()
            .await
            .context("Failed to read SMS 2FA error response text")?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(numbers) = json.get("trustedPhoneNumbers")
        {
            let numbers: Vec<TrustedNumber> = serde_json::from_value(numbers.clone())
                .context("Failed to parse trusted phone numbers")?;
            debug!(
                "Retrieved {} trusted phone numbers (status {}): {:?}",
                numbers.len(),
                status,
                numbers
            );
            return Ok(numbers);
        }

        bail!(
            "Failed to retrieve trusted phone numbers (status {}): {}",
            status,
            text
        );
    }

    fn select_number(&self, selected_number_id: u32) -> Result<LoginState, Report> {
        let numbers = self.trusted_phone_numbers.clone().unwrap_or_default();
        if let Some(number) = numbers.iter().find(|n| n.id == selected_number_id) {
            debug!("Selected trusted number: {}", number.number_with_dial_code);
            return Ok(LoginState::NeedsSMS2FA(number.id));
        }
        bail!("Selected trusted number ID not found in trusted numbers");
    }

    async fn build_2fa_headers(&self, anisette_data: &AnisetteData) -> Result<HeaderMap, Report> {
        let mut headers = anisette_data.get_header_map()?;

        let spd = self
            .spd
            .as_ref()
            .ok_or_else(|| report!("SPD data not available, cannot build 2FA headers"))?;

        let adsid = spd
            .get_str("adsid")
            .context("Failed to build 2FA headers")?;
        let token = spd
            .get_str("GsIdmsToken")
            .context("Failed to build 2FA headers")?;
        let identity = BASE64_STANDARD.encode(format!("{}:{}", adsid, token));

        headers.insert(
            "X-Apple-Identity-Token",
            reqwest::header::HeaderValue::from_str(&identity)?,
        );
        headers.insert(
            "X-Apple-I-MD-RINFO",
            reqwest::header::HeaderValue::from_str(&anisette_data.routing_info)?,
        );

        Ok(headers)
    }

    async fn login_inner(&mut self, password: &str) -> Result<LoginState, Report> {
        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for login")?;

        let gs_service_url = self.grandslam_client.get_url("gsService")?;
        debug!("GrandSlam service URL: {}", gs_service_url);

        let cpd = anisette_data.get_client_provided_data();

        let srp_client = srp::Client::<G2048, Sha256>::new_with_options(false);
        let a: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let a_pub = srp_client.compute_public_ephemeral(&a);

        let req1 = plist!(dict {
            "Header": {
                "Version": "1.0.1"
            },
            "Request": {
                "A2k": a_pub, // A2k = client public ephemeral
                "cpd": cpd.clone(), // cpd = client provided data
                "o": "init", // o = operation
                "ps": [ // ps = protocols supported
                    "s2k",
                    "s2k_fo"
                ],
                "u": self.email.clone(), // u = username
            }
        });

        debug!("Sending initial login request");

        let response = self
            .grandslam_client
            .plist_request(&gs_service_url, &req1, None)
            .await
            .context("Failed to send initial login request")?
            .check_grandslam_error()
            .context("GrandSlam error during initial login request")?;

        debug!("Login step 1 completed");

        let salt = response
            .get_data("s")
            .context("Failed to parse initial login response")?;
        let b_pub = response
            .get_data("B")
            .context("Failed to parse initial login response")?;
        let iters = response
            .get_signed_integer("i")
            .context("Failed to parse initial login response")?;
        let c = response
            .get_str("c")
            .context("Failed to parse initial login response")?;
        let selected_protocol = response
            .get_str("sp")
            .context("Failed to parse initial login response")?;

        debug!(
            "Selected SRP protocol: {}, iterations: {}",
            selected_protocol, iters
        );

        if selected_protocol != "s2k" && selected_protocol != "s2k_fo" {
            bail!("Unsupported SRP protocol selected: {}", selected_protocol);
        }

        let hashed_password = Sha256::digest(password.as_bytes());

        let password_hash = if selected_protocol == "s2k_fo" {
            hex::encode(hashed_password).into_bytes()
        } else {
            hashed_password.to_vec()
        };

        let mut password_buf = [0u8; 32];
        pbkdf2::pbkdf2::<hmac::Hmac<Sha256>>(&password_hash, salt, iters as u32, &mut password_buf)
            .context("Failed to derive password using PBKDF2")?;

        let verifier = srp_client
            .process_reply(&a, self.email.as_bytes(), &password_buf, salt, b_pub)
            .context("Failed to compute SRP proof")?;

        let req2 = plist!(dict {
            "Header": {
                "Version": "1.0.1"
            },
            "Request": {
                "M1": verifier.proof().to_vec(), // A2k = client public ephemeral
                "c": c, // c = client proof from step 1
                "cpd": cpd, // cpd = client provided data
                "o": "complete", // o = operation
                "u": self.email.clone(), // u = username
            }
        });

        debug!("Sending proof login request");

        let mut close_headers = HeaderMap::new();
        close_headers.insert("Connection", HeaderValue::from_static("close"));

        let response2 = self
            .grandslam_client
            .plist_request(&gs_service_url, &req2, Some(close_headers))
            .await
            .context("Failed to send proof login request")?
            .check_grandslam_error()
            .context("GrandSlam error during proof login request")?;

        debug!("Login step 2 response received, verifying server proof");

        let m2 = response2
            .get_data("M2")
            .context("Failed to parse proof login response")?;
        verifier
            .verify_server(m2)
            .map_err(|e| report!("Negotiation failed, server proof mismatch: {}", e))?;

        debug!("Server proof verified");

        let spd_encrypted = response2
            .get_data("spd")
            .context("Failed to get SPD from login response")?;

        let spd_decrypted = Self::decrypt_cbc(&verifier, spd_encrypted)
            .context("Failed to decrypt SPD from login response")?;
        let spd: plist::Dictionary =
            plist::from_bytes(&spd_decrypted).context("Failed to parse decrypted SPD plist")?;

        self.spd = Some(spd);

        let status = response2
            .get_dict("Status")
            .context("Failed to parse proof login response")?;

        debug!("Login step 2 completed");

        if let Some(plist::Value::String(s)) = status.get("au") {
            return Ok(match s.as_str() {
                "trustedDeviceSecondaryAuth" => LoginState::NeedsDevice2FA,
                "secondaryAuth" => LoginState::NeedsSMS2FA(
                    1, /* Just start by trying 1, user can correct after */
                ),
                "repair" => LoginState::LoggedIn, // Just means that you don't have 2FA set up
                unknown => LoginState::NeedsExtraStep(unknown.to_string()),
            });
        }

        Ok(LoginState::LoggedIn)
    }

    pub async fn get_app_token(&mut self, app: &str) -> Result<AppToken, Report> {
        let app = if app.contains("com.apple.gs.") {
            app.to_string()
        } else {
            format!("com.apple.gs.{}", app)
        };

        let anisette_data = self
            .anisette_generator
            .get_anisette_data(self.grandslam_client.clone())
            .await
            .context("Failed to get anisette data for login")?;

        let spd = self
            .spd
            .as_ref()
            .ok_or_else(|| report!("SPD data not available, cannot get app token"))?;

        let dsid = spd.get_str("adsid").context("Failed to get app token")?;
        let auth_token = spd
            .get_str("GsIdmsToken")
            .context("Failed to get app token")?;
        let session_key = spd.get_data("sk").context("Failed to get app token")?;
        let c = spd.get_data("c").context("Failed to get app token")?;

        let checksum = Hmac::<Sha256>::new_from_slice(session_key)
            .context("Failed to create HMAC for app token checksum")
            .attach_with(|| SensitivePlistAttachment::new(spd.clone()))?
            .chain_update("apptokens".as_bytes())
            .chain_update(dsid.as_bytes())
            .chain_update(app.as_bytes())
            .finalize()
            .into_bytes()
            .to_vec();

        let gs_service_url = self.grandslam_client.get_url("gsService")?;
        let cpd = anisette_data.get_client_provided_data();

        let request = plist!(dict {
            "Header": {
                "Version": "1.0.1"
            },
            "Request": {
                "app": [app.clone()],
                "c": c,
                "checksum": checksum,
                "cpd": cpd,
                "o": "apptokens",
                "u": dsid,
                "t": auth_token
            }
        });

        let resp = self
            .grandslam_client
            .plist_request(&gs_service_url, &request, None)
            .await
            .context("Failed to send app token request")?
            .check_grandslam_error()
            .context("GrandSlam error during app token request")?;

        let encrypted_token = resp
            .get_data("et")
            .context("Failed to get encrypted token")?;

        debug!("Acquired encrypted token for {}", app);
        let decrypted_token = Self::decrypt_gcm(encrypted_token, session_key)
            .context("Failed to decrypt app token")?;
        debug!("Decrypted app token for {}", app);

        let token: Dictionary = plist::from_bytes(&decrypted_token)
            .context("Failed to parse decrypted app token plist")?;

        let status = token
            .get_signed_integer("status-code")
            .context("Failed to get status code from app token")?;
        if status != 200 {
            bail!("App token request failed with status code {}", status);
        }
        let token_dict = token
            .get_dict("t")
            .context("Failed to get token dictionary from app token")?;
        let app_token = token_dict
            .get_dict(&app)
            .context("Failed to get app token string")?;

        let app_token = AppToken {
            token: app_token
                .get_str("token")
                .context("Failed to get app token string")?
                .to_string(),
            duration: app_token
                .get_signed_integer("duration")
                .context("Failed to get app token duration")? as u64,
            expiry: app_token
                .get_signed_integer("expiry")
                .context("Failed to get app token expiry")? as u64,
        };

        info!("Successfully retrieved app token for {}", app);

        Ok(app_token)
    }

    fn create_session_key(usr: &ClientVerifier<Sha256>, name: &str) -> Result<Vec<u8>, Report> {
        Ok(Hmac::<Sha256>::new_from_slice(usr.key())?
            .chain_update(name.as_bytes())
            .finalize()
            .into_bytes()
            .to_vec())
    }

    fn decrypt_cbc(usr: &ClientVerifier<Sha256>, data: &[u8]) -> Result<Vec<u8>, Report> {
        let extra_data_key = Self::create_session_key(usr, "extra data key:")?;
        let extra_data_iv = Self::create_session_key(usr, "extra data iv:")?;
        let extra_data_iv = &extra_data_iv[..16];

        Ok(
            cbc::Decryptor::<aes::Aes256>::new_from_slices(&extra_data_key, extra_data_iv)?
                .decrypt_padded_vec::<Pkcs7>(data)?,
        )
    }

    fn decrypt_gcm(data: &[u8], key: &[u8]) -> Result<Vec<u8>, Report> {
        if data.len() < 3 + 16 + 16 {
            bail!(
                "Encrypted token is too short to be valid (only {} bytes)",
                data.len()
            );
        }
        let header = &data[0..3];
        if header != b"XYZ" {
            bail!(
                "Encrypted token is in an unknown format: {}",
                String::from_utf8_lossy(header)
            );
        }
        let iv = &data[3..19];
        let ciphertext_and_tag = &data[19..];

        if key.len() != 32 {
            bail!("Session key is not the correct length: {} bytes", key.len());
        }
        if iv.len() != 16 {
            bail!("IV is not the correct length: {} bytes", iv.len());
        }

        debug!(
            "Decrypting GCM data with key of length {} and IV of length {}",
            key.len(),
            iv.len()
        );
        let key = aes_gcm::Key::<AesGcm<Aes256, U16>>::try_from(key)?;
        debug!("GCM key created successfully");
        let cipher = AesGcm::<Aes256, U16>::new(&key);
        debug!("GCM cipher initialized successfully");
        let nonce = Nonce::<U16>::try_from(iv)?;
        debug!("GCM nonce created successfully");

        let mut buf = ciphertext_and_tag.to_vec();

        cipher
            .decrypt_in_place(&nonce, header, &mut buf)
            .map_err(|e| report!("Failed to decrypt gcm: {}", e))?;
        debug!("GCM decryption successful");

        Ok(buf)
    }
}

impl std::fmt::Display for AppleAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Apple Account: ")?;
        match self.get_name() {
            Ok((first, last)) => write!(f, "{} {} ", first, last),
            Err(_) => Ok(()),
        }?;
        write!(f, "{} ({:?})", self.email, self.login_state)
    }
}

#[derive(Debug, Clone)]
pub struct AppToken {
    pub token: String,
    pub duration: u64,
    pub expiry: u64,
}

fn censor_email(email: &str) -> String {
    if std::env::var("DEBUG_SENSITIVE").is_ok() {
        return email.to_string();
    }
    if let Some(at_pos) = email.find('@') {
        let (local, domain) = email.split_at(at_pos);
        if local.len() <= 2 {
            format!("{}***{}", &local[0..1], &domain)
        } else {
            format!(
                "{}***{}{}",
                &local[0..1],
                &local[local.len() - 1..],
                &domain
            )
        }
    } else {
        "***".to_string()
    }
}
