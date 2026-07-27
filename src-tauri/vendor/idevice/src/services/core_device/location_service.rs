// Jackson Coxson

use serde::Deserialize;

use crate::{IdeviceError, ReadWrite, RsdService, obf};

use super::CoreDeviceError;

/// A built-in location-simulation scenario the device offers.
#[derive(Debug, Clone, Deserialize)]
pub struct LocationScenario {
    /// The scenario's identifier, e.g. `"City Run"`.
    pub name: String,
    /// The display name (often the same as `name`).
    #[serde(rename = "localizedName")]
    pub localized_name: String,
}

impl RsdService for LocationServiceClient<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.coredevice.locationservice")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Ok(Self {
            inner: super::CoreDeviceServiceClient::new(stream).await?,
        })
    }
}

#[derive(Debug)]
pub struct LocationServiceClient<R: ReadWrite> {
    inner: super::CoreDeviceServiceClient<R>,
}

impl<R: ReadWrite> LocationServiceClient<R> {
    pub fn new(inner: super::CoreDeviceServiceClient<R>) -> Self {
        Self { inner }
    }

    /// List the device's built-in location-simulation scenarios.
    pub async fn available_location_scenarios(
        &mut self,
    ) -> Result<Vec<LocationScenario>, IdeviceError> {
        let res = self
            .inner
            .invoke_with_plist_action(
                obf!("com.apple.coredevice.feature.simulatelocation"),
                plist::Dictionary::new(),
                obf!("com.apple.coredevice.action.availablelocationscenarios"),
            )
            .await?;

        let scenarios = res
            .as_dictionary()
            .and_then(|d| d.get("scenarios"))
            .ok_or(CoreDeviceError::MissingField("scenarios"))?;
        plist::from_value(scenarios)
            .map_err(|_| CoreDeviceError::MalformedField("scenarios").into())
    }
}
