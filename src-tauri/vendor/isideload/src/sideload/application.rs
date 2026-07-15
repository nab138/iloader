// This file was made using https://github.com/Dadoum/Sideloader as a reference.
// I'm planning on redoing this later to better handle entitlements, extensions, etc, but it will do for now

use crate::SideloadError;
use crate::dev::app_ids::{AppId, AppIdsApi};
use crate::dev::developer_session::DeveloperSession;
use crate::dev::teams::DeveloperTeam;
use crate::sideload::bundle::Bundle;
use crate::sideload::cert_identity::CertificateIdentity;
use rootcause::option_ext::OptionExt;
use rootcause::prelude::*;
use std::fs::File;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tracing::info;
use zip::ZipArchive;

pub struct Application {
    pub bundle: Bundle,
    //pub temp_path: PathBuf,
}

impl Application {
    pub fn new(path: PathBuf) -> Result<Self, Report> {
        if !path.exists() {
            bail!(SideloadError::InvalidBundle(
                "Application path does not exist".to_string(),
            ));
        }

        let mut bundle_path = path.clone();
        //let mut temp_path = PathBuf::new();

        if path.is_file() {
            let temp_dir = std::env::temp_dir();
            let temp_path = temp_dir.join(
                path.file_name()
                    .ok_or_report()?
                    .to_string_lossy()
                    .to_string()
                    + "_extracted",
            );
            if temp_path.exists() {
                std::fs::remove_dir_all(&temp_path)
                    .context("Failed to remove existing temporary directory")?;
            }
            std::fs::create_dir_all(&temp_path).context("Failed to create temporary directory")?;

            let file = File::open(&path).context("Failed to open application archive")?;
            let mut archive =
                ZipArchive::new(file).context("Failed to open application archive")?;
            archive
                .extract(&temp_path)
                .context("Failed to extract application archive")?;

            let payload_folder = temp_path.join("Payload");
            if payload_folder.exists() && payload_folder.is_dir() {
                let app_dirs: Vec<_> = std::fs::read_dir(&payload_folder)
                    .context("Failed to read Payload directory")?
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "app"))
                    .collect();
                if app_dirs.len() == 1 {
                    bundle_path = app_dirs[0].path();
                } else if app_dirs.is_empty() {
                    bail!(SideloadError::InvalidBundle(
                        "No .app directory found in Payload".to_string(),
                    ));
                } else {
                    bail!(SideloadError::InvalidBundle(
                        "Multiple .app directories found in Payload".to_string(),
                    ));
                }
            } else {
                bail!(SideloadError::InvalidBundle(
                    "No Payload directory found in the application archive".to_string(),
                ));
            }
        }
        let bundle = Bundle::new(bundle_path)?;

        Ok(Application {
            bundle, /*temp_path*/
        })
    }

    pub fn get_special_app(&self) -> Option<SpecialApp> {
        let bundle_id = self.bundle.bundle_identifier().unwrap_or("");
        let special_app = match bundle_id {
            "com.rileytestut.AltStore" => Some(SpecialApp::AltStore),
            "com.SideStore.SideStore" => Some(SpecialApp::SideStore),
            "app.stik.store" => Some(SpecialApp::StikStore),
            _ => None,
        };
        if special_app.is_some() {
            return special_app;
        }

        if self
            .bundle
            .frameworks()
            .iter()
            .any(|f| f.bundle_identifier().unwrap_or("") == "com.SideStore.SideStore")
        {
            return Some(SpecialApp::SideStoreLc);
        }

        if bundle_id == "com.kdt.livecontainer" {
            return Some(SpecialApp::LiveContainer);
        }

        None
    }

    pub fn main_bundle_id(&self) -> Result<String, Report> {
        let str = self
            .bundle
            .bundle_identifier()
            .ok_or_report()
            .context("Failed to get main bundle identifier")?
            .to_string();

        Ok(str)
    }

    pub fn main_app_name(&self) -> Result<String, Report> {
        let str = self
            .bundle
            .bundle_name()
            .ok_or_report()
            .context("Failed to get main app name")?
            .to_string();

        Ok(str)
    }

    pub fn update_bundle_id(
        &mut self,
        main_app_bundle_id: &str,
        main_app_id_str: &str,
    ) -> Result<(), Report> {
        let extensions = self.bundle.app_extensions_mut();
        for ext in extensions.iter_mut() {
            if let Some(id) = ext.bundle_identifier() {
                if !(id.starts_with(main_app_bundle_id) && id.len() > main_app_bundle_id.len()) {
                    bail!(SideloadError::InvalidBundle(format!(
                        "Extension {} is not part of the main app bundle identifier: {}",
                        ext.bundle_name().unwrap_or("Unknown"),
                        id
                    )));
                } else {
                    ext.set_bundle_identifier(&format!(
                        "{}{}",
                        main_app_id_str,
                        &id[main_app_bundle_id.len()..]
                    ));
                }
            }
        }
        self.bundle.set_bundle_identifier(main_app_id_str);

        Ok(())
    }

    pub async fn register_app_ids(
        &self,
        //mode: &ExtensionsBehavior,
        dev_session: &mut DeveloperSession,
        team: &DeveloperTeam,
    ) -> Result<Vec<AppId>, Report> {
        let extension_refs: Vec<_> = self.bundle.app_extensions().iter().collect();
        let mut bundles_with_app_id = vec![&self.bundle];
        bundles_with_app_id.extend(extension_refs);

        let list_app_ids_response = dev_session
            .list_app_ids(team, None)
            .await
            .context("Failed to list app IDs for the developer team")?;
        let app_ids_to_register = bundles_with_app_id
            .iter()
            .filter(|bundle| {
                let bundle_id = bundle.bundle_identifier().unwrap_or("");
                !list_app_ids_response
                    .app_ids
                    .iter()
                    .any(|app_id| app_id.identifier == bundle_id)
            })
            .collect::<Vec<_>>();

        if let Some(available) = list_app_ids_response.available_quantity
            && app_ids_to_register.len() > available.try_into()?
        {
            bail!(
                "Not enough available app IDs. {} are required, but only {} are available.",
                app_ids_to_register.len(),
                available
            );
        }

        for bundle in app_ids_to_register {
            let id = bundle.bundle_identifier().unwrap_or("");
            let name = bundle.bundle_name().unwrap_or("");
            dev_session.add_app_id(team, name, id, None).await?;
        }
        let list_app_id_response = dev_session.list_app_ids(team, None).await?;
        let app_ids: Vec<_> = list_app_id_response
            .app_ids
            .into_iter()
            .filter(|app_id| {
                bundles_with_app_id
                    .iter()
                    .any(|bundle| app_id.identifier == bundle.bundle_identifier().unwrap_or(""))
            })
            .collect();

        info!("Registered app IDs");
        Ok(app_ids)
    }

    pub async fn apply_special_app_behavior(
        &mut self,
        special: &Option<SpecialApp>,
        group_identifier: &str,
        cert: &CertificateIdentity,
    ) -> Result<(), Report> {
        let Some(special) = special.as_ref() else {
            return Ok(());
        };

        if matches!(
            special,
            SpecialApp::SideStoreLc
                | SpecialApp::SideStore
                | SpecialApp::AltStore
                | SpecialApp::StikStore
        ) {
            if !matches!(special, SpecialApp::StikStore) {
                self.bundle.app_info.insert(
                    "ALTAppGroups".to_string(),
                    plist::Value::Array(vec![plist::Value::String(group_identifier.to_string())]),
                );
            }
            info!("Injecting certificate for {}", special);

            let target_bundle =
                match special {
                    SpecialApp::SideStoreLc => self.bundle.frameworks_mut().iter_mut().find(|fw| {
                        fw.bundle_identifier().unwrap_or("") == "com.SideStore.SideStore"
                    }),
                    _ => Some(&mut self.bundle),
                };

            if let Some(target_bundle) = target_bundle {
                let id_key = match special {
                    SpecialApp::StikStore => "MachineID",
                    _ => "ALTCertificateID",
                };
                let cert_file_name = match special {
                    SpecialApp::StikStore => "Certificate.p12",
                    _ => "ALTCertificate.p12",
                };
                target_bundle.app_info.insert(
                    id_key.to_string(),
                    plist::Value::String(cert.get_serial_number()),
                );

                let p12_bytes = cert
                    .as_p12(&cert.machine_id)
                    .await
                    .context("Failed to encode cert as p12")?;
                let alt_cert_path = target_bundle.bundle_dir.join(cert_file_name);

                let mut file = tokio::fs::File::create(&alt_cert_path)
                    .await
                    .context(format!("Failed to create {}", cert_file_name))?;
                file.write_all(&p12_bytes)
                    .await
                    .context(format!("Failed to write {}", cert_file_name))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialApp {
    SideStore,
    SideStoreLc,
    LiveContainer,
    AltStore,
    StikStore,
}

// impl display
impl std::fmt::Display for SpecialApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecialApp::SideStore => write!(f, "SideStore"),
            SpecialApp::SideStoreLc => write!(f, "SideStore+LiveContainer"),
            SpecialApp::LiveContainer => write!(f, "LiveContainer"),
            SpecialApp::AltStore => write!(f, "AltStore"),
            SpecialApp::StikStore => write!(f, "StikStore"),
        }
    }
}
