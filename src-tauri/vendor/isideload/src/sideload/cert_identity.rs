use apple_codesign::{
    SigningSettings,
    cryptography::{InMemoryPrivateKey, PrivateKey},
};
use hex::ToHex;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_RSA_SHA256};
use rootcause::{option_ext::OptionExt, prelude::*};
use rsa::{
    RsaPrivateKey,
    pkcs1::EncodeRsaPublicKey,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding},
};

use sha1::Sha1;
use sha2::{Digest, Sha256};
use tracing::{error, info};
use x509_certificate::CapturedX509Certificate;

use crate::{
    SideloadError,
    dev::{
        certificates::{CertificatesApi, DevelopmentCertificate},
        developer_session::DeveloperSession,
        teams::DeveloperTeam,
    },
    sideload::builder::MaxCertsBehavior,
    util::storage::SideloadingStorage,
};

pub struct CertificateIdentity {
    pub machine_id: String,
    pub machine_name: String,
    pub certificate: CapturedX509Certificate,
    pub private_key: RsaPrivateKey,
    pub signing_key: InMemoryPrivateKey,
}

impl CertificateIdentity {
    // This implementation was mostly borrowed from Impactor (https://github.com/khcrysalis/Impactor/blob/main/crates/plume_core/src/utils/certificate.rs)
    /// Exports the certificate and private key as a PKCS#12 archive
    /// If you plan to import into SideStore/AltStore, use the machine id as the password
    pub async fn as_p12(&self, password: &str) -> Result<Vec<u8>, Report> {
        let cert_der = self.certificate.encode_der()?;
        let cert_der_len = cert_der.len();
        let key_der = self.private_key.to_pkcs8_der()?.as_bytes().to_vec();
        let key_der_len = key_der.len();

        let cert = p12_keystore::Certificate::from_der(&cert_der)
            .map_err(|e| report!("Failed to parse certificate: {:?}", e))?;
        let cert_subject = cert.subject().to_string();
        let cert_issuer = cert.issuer().to_string();

        let local_key_id = {
            let mut hasher = Sha1::new();
            hasher.update(&key_der);
            let hash = hasher.finalize();
            hash[..8].to_vec()
        };

        let key_chain = p12_keystore::PrivateKeyChain::new(
            local_key_id,
            p12_keystore::PrivateKey::from_der(&key_der)?,
            vec![cert],
        );

        let mut keystore = p12_keystore::KeyStore::new();
        keystore.add_entry(
            "isideload",
            p12_keystore::KeyStoreEntry::PrivateKeyChain(key_chain),
        );

        let writer = keystore.writer(password);
        match writer.write() {
            Ok(p12) => Ok(p12),
            Err(e) => {
                let subject_codepoints = cert_subject
                    .chars()
                    .map(|c| format!("U+{:04X}", c as u32))
                    .collect::<Vec<_>>()
                    .join(" ");
                let has_non_bmp_subject_chars = cert_subject.chars().any(|c| (c as u32) > 0xFFFF);

                error!(
                    cert_subject = %cert_subject,
                    cert_issuer = %cert_issuer,
                    cert_subject_codepoints = %subject_codepoints,
                    has_non_bmp_subject_chars,
                    cert_der_len,
                    key_der_len,
                    password_char_len = password.chars().count(),
                    "Failed to write PKCS#12 archive"
                );

                let err = format!("Failed to write PKCS#12 archive: {:?}", e);
                Err(e).context(err)?
            }
        }
    }

    pub fn get_serial_number(&self) -> String {
        let serial: String = self.certificate.serial_number_asn1().encode_hex();
        serial.trim_start_matches('0').to_string().to_uppercase()
    }

    pub async fn retrieve(
        machine_name: &str,
        apple_email: &str,
        developer_session: &mut DeveloperSession,
        team: &DeveloperTeam,
        storage: &dyn SideloadingStorage,
        max_certs_behavior: &MaxCertsBehavior,
    ) -> Result<Self, Report> {
        let pr = Self::retrieve_private_key(apple_email, storage).await?;
        let signing_key = Self::build_signing_key(&pr)?;

        let found = Self::find_matching(&pr, machine_name, developer_session, team).await;
        if let Ok(Some((cert, x509_cert))) = found {
            info!("Found matching certificate");
            return Ok(Self {
                machine_id: cert.machine_id.clone().unwrap_or_default(),
                machine_name: cert.machine_name.clone().unwrap_or_default(),
                certificate: x509_cert,
                private_key: pr,
                signing_key,
            });
        }

        if let Err(e) = found {
            error!("Failed to check for matching certificate: {:?}", e);
        }
        info!("Requesting new certificate");
        let (cert, x509_cert) = Self::request_certificate(
            &pr,
            machine_name.to_string(),
            developer_session,
            team,
            max_certs_behavior,
        )
        .await?;

        info!("Successfully obtained certificate");

        Ok(Self {
            machine_id: cert.machine_id.clone().unwrap_or_default(),
            machine_name: cert.machine_name.clone().unwrap_or_default(),
            certificate: x509_cert,
            private_key: pr,
            signing_key,
        })
    }

    async fn retrieve_private_key(
        apple_email: &str,
        storage: &dyn SideloadingStorage,
    ) -> Result<RsaPrivateKey, Report> {
        let mut hasher = Sha256::new();
        hasher.update(apple_email.as_bytes());
        let email_hash = hex::encode(hasher.finalize());

        let private_key = storage.retrieve_data(&format!("{}/key", email_hash))?;
        if let Some(priv_key) = private_key {
            info!("Using existing private key from storage");
            return Ok(RsaPrivateKey::from_pkcs8_der(&priv_key)?);
        }

        let mut rng = rand::rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048)?;
        storage.store_data(
            &format!("{}/key", email_hash),
            private_key.to_pkcs8_der()?.as_bytes(),
        )?;

        Ok(private_key)
    }

    async fn find_matching(
        private_key: &RsaPrivateKey,
        machine_name: &str,
        developer_session: &mut DeveloperSession,
        team: &DeveloperTeam,
    ) -> Result<Option<(DevelopmentCertificate, CapturedX509Certificate)>, Report> {
        let public_key_der = private_key
            .to_public_key()
            .to_pkcs1_der()?
            .as_bytes()
            .to_vec();
        for cert in developer_session
            .list_ios_certs(team)
            .await?
            .iter()
            .filter(|c| {
                c.cert_content.is_some()
                    && c.machine_name.as_deref().unwrap_or("") == machine_name
                    && c.machine_id.is_some()
            })
        {
            let x509_cert = CapturedX509Certificate::from_der(
                cert.cert_content.as_ref().ok_or_report()?.as_ref(),
            )?;

            if public_key_der == x509_cert.public_key_data().as_ref() {
                return Ok(Some((cert.clone(), x509_cert)));
            }
        }

        Ok(None)
    }

    async fn request_certificate(
        private_key: &RsaPrivateKey,
        machine_name: String,
        developer_session: &mut DeveloperSession,
        team: &DeveloperTeam,
        max_certs_behavior: &MaxCertsBehavior,
    ) -> Result<(DevelopmentCertificate, CapturedX509Certificate), Report> {
        let csr = Self::build_csr(private_key).context("Failed to generate CSR")?;

        let mut i = 0;
        let mut existing_certs: Option<Vec<DevelopmentCertificate>> = None;

        while i < 4 {
            i += 1;

            let result = developer_session
                .submit_development_csr(team, csr.clone(), machine_name.clone(), None)
                .await;

            match result {
                Ok(request) => {
                    let apple_certs = developer_session.list_ios_certs(team).await?;

                    let apple_cert = apple_certs
                        .iter()
                        .find(|c| c.certificate_id == Some(request.cert_request_id.clone()))
                        .ok_or_else(|| {
                            report!("Failed to find certificate after submitting CSR")
                        })?;

                    let x509_cert = CapturedX509Certificate::from_der(
                        apple_cert
                            .cert_content
                            .as_ref()
                            .ok_or_else(|| report!("Certificate content missing"))?
                            .as_ref(),
                    )?;

                    return Ok((apple_cert.clone(), x509_cert));
                }
                Err(e) => {
                    let error = e
                        .iter_reports()
                        .find_map(|node| node.downcast_current_context::<SideloadError>());
                    if let Some(SideloadError::DeveloperError(code, _)) = error {
                        if *code == 7460 {
                            if existing_certs.is_none() {
                                existing_certs = Some(
                                    developer_session
                                        .list_ios_certs(team)
                                        .await?
                                        .iter()
                                        .filter(|c| c.serial_number.is_some())
                                        .cloned()
                                        .collect(),
                                );
                            }
                            Self::revoke_others(
                                developer_session,
                                team,
                                max_certs_behavior,
                                SideloadError::DeveloperError(
                                    *code,
                                    "Maximum number of certificates reached".to_string(),
                                ),
                                existing_certs.as_mut().ok_or_report()?,
                            )
                            .await?;
                        } else {
                            return Err(e);
                        }
                    }
                }
            };
        }

        Err(report!("Reached max attempts to request certificate"))
    }

    fn build_csr(private_key: &RsaPrivateKey) -> Result<String, Report> {
        let mut params = CertificateParams::new(vec![])?;
        let mut dn = DistinguishedName::new();

        dn.push(DnType::CountryName, "US");
        dn.push(DnType::StateOrProvinceName, "STATE");
        dn.push(DnType::LocalityName, "LOCAL");
        dn.push(DnType::OrganizationName, "ORGNIZATION");
        dn.push(DnType::CommonName, "CN");
        params.distinguished_name = dn;

        let subject_key = KeyPair::from_pkcs8_pem_and_sign_algo(
            &private_key.to_pkcs8_pem(LineEnding::LF)?,
            &PKCS_RSA_SHA256,
        )?;

        Ok(params.serialize_request(&subject_key)?.pem()?)
    }

    fn build_signing_key(private_key: &RsaPrivateKey) -> Result<InMemoryPrivateKey, Report> {
        let pkcs8 = private_key.to_pkcs8_der()?;
        Ok(InMemoryPrivateKey::from_pkcs8_der(pkcs8.as_bytes())?)
    }

    async fn revoke_others(
        developer_session: &mut DeveloperSession,
        team: &DeveloperTeam,
        max_certs_behavior: &MaxCertsBehavior,
        error: SideloadError,
        existing_certs: &mut Vec<DevelopmentCertificate>,
    ) -> Result<(), Report> {
        match max_certs_behavior {
            MaxCertsBehavior::Revoke => {
                if let Some(cert) = existing_certs.pop() {
                    info!(
                        "Revoking certificate with name: {:?} ({:?})",
                        cert.name, cert.machine_name
                    );
                    developer_session
                        .revoke_development_cert(team, &cert.serial_number.ok_or_report()?, None)
                        .await?;
                    Ok(())
                } else {
                    error!("No more certificates to revoke but still hitting max certs error");
                    Err(error.into())
                }
            }
            MaxCertsBehavior::Error => Err(error.into()),
            MaxCertsBehavior::Prompt(prompt_fn) => {
                let certs_to_revoke = prompt_fn(existing_certs);
                if certs_to_revoke.is_none() {
                    error!("User did not select any certificates to revoke");
                    return Err(error.into());
                }
                for serial in certs_to_revoke.ok_or_report()? {
                    info!("Revoking certificate with serial number: {}", serial);
                    developer_session
                        .revoke_development_cert(team, &serial, None)
                        .await?;
                    existing_certs.retain(|c| c.serial_number != Some(serial.clone()));
                }
                Ok(())
            }
        }
    }

    pub fn setup_signing_settings<'a>(
        &'a self,
        settings: &mut SigningSettings<'a>,
    ) -> Result<(), Report> {
        settings.set_signing_key(
            self.signing_key.as_key_info_signer(),
            self.certificate.clone(),
        );
        settings.chain_apple_certificates();
        settings.set_team_id_from_signing_certificate();

        Ok(())
    }
}
