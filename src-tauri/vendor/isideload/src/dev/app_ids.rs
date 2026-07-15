use crate::{
    dev::{
        developer_session::DeveloperSession,
        device_type::{DeveloperDeviceType, dev_url},
        teams::DeveloperTeam,
    },
    util::plist::{PlistDataExtract, SensitivePlistAttachment},
};
use plist::{Data, Date, Dictionary, Value};
use plist_macro::plist;
use reqwest::header::HeaderValue;
use rootcause::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppId {
    pub app_id_id: String,
    pub identifier: String,
    pub name: String,
    pub features: Dictionary,
    pub expiration_date: Option<Date>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAppIdsResponse {
    pub app_ids: Vec<AppId>,
    pub max_quantity: Option<u64>,
    pub available_quantity: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub encoded_profile: Data,
    pub filename: String,
    pub provisioning_profile_id: String,
    pub name: String,
    pub status: String,
    pub r#type: String,
    pub distribution_method: String,
    pub pro_pro_platorm: Option<String>,
    #[serde(rename = "UUID")]
    pub uuid: String,
    pub date_expire: Date,
    pub managing_app: Option<String>,
    pub app_id_id: String,
    pub is_template_profile: bool,
    pub is_team_profile: Option<bool>,
    pub is_free_provisioning_profile: Option<bool>,
}

#[async_trait::async_trait]
pub trait AppIdsApi {
    fn developer_session(&mut self) -> &mut DeveloperSession;

    async fn add_app_id(
        &mut self,
        team: &DeveloperTeam,
        name: &str,
        identifier: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<AppId, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "identifier": identifier,
            "name": name,
        });

        let app_id: AppId = self
            .developer_session()
            .send_dev_request(&dev_url("addAppId", device_type), body, "appId")
            .await
            .context("Failed to add developer app ID")?;

        Ok(app_id)
    }

    async fn list_app_ids(
        &mut self,
        team: &DeveloperTeam,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<ListAppIdsResponse, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
        });

        let response: Value = self
            .developer_session()
            .send_dev_request_no_response(&dev_url("listAppIds", device_type), body)
            .await
            .context("Failed to list developer app IDs")?
            .into();

        let app_ids: ListAppIdsResponse = plist::from_value(&response).map_err(|e| {
            report!("Failed to deserialize app id response: {:?}", e).attach(
                SensitivePlistAttachment::new(
                    response
                        .as_dictionary()
                        .unwrap_or(&Dictionary::new())
                        .clone(),
                ),
            )
        })?;

        Ok(app_ids)
    }

    async fn update_app_id(
        &mut self,
        team: &DeveloperTeam,
        app_id: &AppId,
        features: Dictionary,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<AppId, Report> {
        let mut body = plist!(dict {
            "teamId": &team.team_id,
            "appIdId": &app_id.app_id_id
        });

        for (key, value) in features {
            body.insert(key.clone(), value.clone());
        }

        Ok(self
            .developer_session()
            .send_dev_request(&dev_url("updateAppId", device_type), body, "appId")
            .await
            .context("Failed to update developer app ID")?)
    }

    async fn delete_app_id(
        &mut self,
        team: &DeveloperTeam,
        app_id_id: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<(), Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "appIdId": app_id_id,
        });

        self.developer_session()
            .send_dev_request_no_response(&dev_url("deleteAppId", device_type), body)
            .await
            .context("Failed to delete developer app ID")?;

        Ok(())
    }

    async fn download_team_provisioning_profile(
        &mut self,
        team: &DeveloperTeam,
        app_id: &AppId,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<Profile, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "appIdId": &app_id.app_id_id,
        });

        let response: Profile = self
            .developer_session()
            .send_dev_request(
                &dev_url("downloadTeamProvisioningProfile", device_type),
                body,
                "provisioningProfile",
            )
            .await
            .context("Failed to download provisioning profile")?;

        Ok(response)
    }

    async fn add_increased_memory_limit(
        &mut self,
        team: &DeveloperTeam,
        app_id: &AppId,
    ) -> Result<(), Report> {
        let dev_session = self.developer_session();

        let mut headers = dev_session
            .get_headers()
            .await
            .context("Failed to get anisette headers")?;
        headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/vnd.api+json"),
        );
        headers.insert(
            "Accept",
            HeaderValue::from_static("application/vnd.api+json"),
        );

        dev_session
                .get_grandslam_client()
                .patch(&format!(
                    "https://developerservices2.apple.com/services/v1/bundleIds/{}",
                    app_id.app_id_id
                ))?
                .headers(headers)
                .body(format!(
                "{{\"data\":{{\"relationships\":{{\"bundleIdCapabilities\":{{\"data\":[{{\"relationships\":{{\"capability\":{{\"data\":{{\"id\":\"INCREASED_MEMORY_LIMIT\",\"type\":\"capabilities\"}}}}}},\"type\":\"bundleIdCapabilities\",\"attributes\":{{\"settings\":[],\"enabled\":true}}}}]}}}},\"id\":\"{}\",\"attributes\":{{\"hasExclusiveManagedCapabilities\":false,\"teamId\":\"{}\",\"bundleType\":\"bundle\",\"identifier\":\"{}\",\"seedId\":\"{}\",\"name\":\"{}\"}},\"type\":\"bundleIds\"}}}}",
                app_id.app_id_id, team.team_id, app_id.identifier, team.team_id, app_id.name
            ))
                .send()
                .await.context("Failed to request increased memory entitlement")?
                .error_for_status().context("Failed to add increased memory entitlement")?;

        Ok(())
    }
}

impl AppIdsApi for DeveloperSession {
    fn developer_session(&mut self) -> &mut DeveloperSession {
        self
    }
}

impl AppId {
    pub async fn ensure_group_feature(
        &mut self,
        dev_session: &mut DeveloperSession,
        team: &DeveloperTeam,
    ) -> Result<(), Report> {
        let app_group_feature_enabled = self.features.get_bool("APG3427HIY")?;

        if !app_group_feature_enabled {
            let body = plist!(dict {
                "APG3427HIY": true,
            });
            let new_features = dev_session
                .update_app_id(team, self, body, None)
                .await?
                .features;
            self.features = new_features;
        }

        Ok(())
    }
}
