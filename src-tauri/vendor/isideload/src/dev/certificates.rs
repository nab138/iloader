use crate::dev::{
    developer_session::DeveloperSession,
    device_type::{DeveloperDeviceType, dev_url},
    teams::DeveloperTeam,
};
use plist::{Data, Date};
use plist_macro::plist;
use rootcause::prelude::*;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DevelopmentCertificate {
    pub name: Option<String>,
    pub certificate_id: Option<String>,
    pub serial_number: Option<String>,
    pub machine_id: Option<String>,
    pub machine_name: Option<String>,
    pub cert_content: Option<Data>,
    pub certificate_platform: Option<String>,
    pub certificate_type: Option<CertificateType>,
    pub status: Option<String>,
    pub status_code: Option<i64>,
    pub expiration_date: Option<Date>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CertificateType {
    pub certificate_type_display_id: Option<String>,
    pub name: Option<String>,
    pub platform: Option<String>,
    pub permission_type: Option<String>,
    pub distribution_type: Option<String>,
    pub distribution_method: Option<String>,
    pub owner_type: Option<String>,
    pub days_overlap: Option<i64>,
    pub max_active_certs: Option<i64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CertRequest {
    pub cert_request_id: String,
}

// the automatic debug implementation spams the console with the cert content bytes
impl std::fmt::Debug for DevelopmentCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("DevelopmentCertificate");
        s.field("name", &self.name)
            .field("certificate_id", &self.certificate_id)
            .field("serial_number", &self.serial_number)
            .field("machine_id", &self.machine_id)
            .field("machine_name", &self.machine_name)
            .field(
                "cert_content",
                &self
                    .cert_content
                    .as_ref()
                    .map(|c| format!("Some([{} bytes])", c.as_ref().len()))
                    .unwrap_or("None".to_string()),
            )
            .field("certificate_platform", &self.certificate_platform)
            .field("certificate_type", &self.certificate_type)
            .field("status", &self.status)
            .field("status_code", &self.status_code)
            .field("expiration_date", &self.expiration_date)
            .finish()
    }
}

#[async_trait::async_trait]
pub trait CertificatesApi {
    fn developer_session(&mut self) -> &mut DeveloperSession;

    async fn list_all_development_certs(
        &mut self,
        team: &DeveloperTeam,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<Vec<DevelopmentCertificate>, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
        });

        let certs: Vec<DevelopmentCertificate> = self
            .developer_session()
            .send_dev_request(
                &dev_url("listAllDevelopmentCerts", device_type),
                body,
                "certificates",
            )
            .await
            .context("Failed to list development certificates")?;

        Ok(certs)
    }

    async fn list_ios_certs(
        &mut self,
        team: &DeveloperTeam,
    ) -> Result<Vec<DevelopmentCertificate>, Report> {
        let certs = self
            .list_all_development_certs(team, DeveloperDeviceType::Ios)
            .await?;

        Ok(certs
            .into_iter()
            .filter(|c| {
                if let Some(platform) = &c.certificate_platform {
                    platform.to_lowercase() == "ios"
                } else if let Some(cert_type) = &c.certificate_type {
                    if let Some(platform) = &cert_type.platform {
                        platform.to_lowercase() == "ios"
                    } else {
                        // I don't know how consistently these field is populated because apple apis are stupid, and I don't want to break things so just assume
                        true
                    }
                } else {
                    true
                }
            })
            .collect())
    }

    async fn revoke_development_cert(
        &mut self,
        team: &DeveloperTeam,
        serial_number: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<(), Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "serialNumber": serial_number,
        });

        self.developer_session()
            .send_dev_request_no_response(
                &dev_url("revokeDevelopmentCert", device_type),
                Some(body),
            )
            .await
            .context("Failed to revoke development certificate")?;

        Ok(())
    }

    async fn submit_development_csr(
        &mut self,
        team: &DeveloperTeam,
        csr_content: String,
        machine_name: String,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<CertRequest, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "csrContent": csr_content,
            "machineName": machine_name,
            "machineId": Uuid::new_v4().to_string().to_uppercase(),
        });

        let cert: CertRequest = self
            .developer_session()
            .send_dev_request(
                &dev_url("submitDevelopmentCSR", device_type),
                body,
                "certRequest",
            )
            .await
            .context("Failed to submit development CSR")?;

        Ok(cert)
    }
}

impl CertificatesApi for DeveloperSession {
    fn developer_session(&mut self) -> &mut DeveloperSession {
        self
    }
}
