use crate::dev::{
    app_ids::AppId,
    developer_session::DeveloperSession,
    device_type::{DeveloperDeviceType, dev_url},
    teams::DeveloperTeam,
};
use plist_macro::plist;
use rootcause::prelude::*;
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppGroup {
    pub name: Option<String>,
    pub identifier: String,
    pub application_group: String,
}

#[async_trait::async_trait]
pub trait AppGroupsApi {
    fn developer_session(&mut self) -> &mut DeveloperSession;

    async fn list_app_groups(
        &mut self,
        team: &DeveloperTeam,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<Vec<AppGroup>, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
        });

        let app_groups: Vec<AppGroup> = self
            .developer_session()
            .send_dev_request(
                &dev_url("listApplicationGroups", device_type),
                body,
                "applicationGroupList",
            )
            .await
            .context("Failed to list developer app groups")?;

        Ok(app_groups)
    }

    async fn add_app_group(
        &mut self,
        team: &DeveloperTeam,
        name: &str,
        identifier: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<AppGroup, Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "name": name,
            "identifier": identifier,
        });

        let app_group: AppGroup = self
            .developer_session()
            .send_dev_request(
                &dev_url("addApplicationGroup", device_type),
                body,
                "applicationGroup",
            )
            .await
            .context("Failed to add developer app group")?;

        Ok(app_group)
    }

    async fn assign_app_group(
        &mut self,
        team: &DeveloperTeam,
        app_group: &AppGroup,
        app_id: &AppId,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<(), Report> {
        let body = plist!(dict {
            "teamId": &team.team_id,
            "applicationGroups": &app_group.application_group,
            "appIdId": &app_id.app_id_id,
        });

        self.developer_session()
            .send_dev_request_no_response(
                &dev_url("assignApplicationGroupToAppId", device_type),
                body,
            )
            .await
            .context("Failed to assign developer app group")?;

        Ok(())
    }

    async fn ensure_app_group(
        &mut self,
        team: &DeveloperTeam,
        name: &str,
        identifier: &str,
        device_type: impl Into<Option<DeveloperDeviceType>> + Send,
    ) -> Result<AppGroup, Report> {
        let device_type = device_type.into();
        let groups = self.list_app_groups(team, device_type.clone()).await?;
        let matching_group = groups.iter().find(|g| g.identifier == identifier);

        if let Some(group) = matching_group {
            Ok(group.clone())
        } else {
            info!("Adding application group");
            let group = self
                .add_app_group(team, name, identifier, device_type)
                .await?;
            Ok(group)
        }
    }
}

impl AppGroupsApi for DeveloperSession {
    fn developer_session(&mut self) -> &mut DeveloperSession {
        self
    }
}
