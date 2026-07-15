#[derive(Debug, Clone)]
pub enum DeveloperDeviceType {
    Any,
    Ios,
    Tvos,
    Watchos,
}

impl DeveloperDeviceType {
    pub fn url_segment(&self) -> &'static str {
        match self {
            DeveloperDeviceType::Any => "",
            DeveloperDeviceType::Ios => "ios/",
            DeveloperDeviceType::Tvos => "tvos/",
            DeveloperDeviceType::Watchos => "watchos/",
        }
    }
}

pub fn dev_url(endpoint: &str, device_type: impl Into<Option<DeveloperDeviceType>>) -> String {
    format!(
        "https://developerservices2.apple.com/services/QH65B2/{}{}.action?clientId=XABBG36SBA",
        device_type
            .into()
            .unwrap_or(DeveloperDeviceType::Ios)
            .url_segment(),
        endpoint,
    )
}
