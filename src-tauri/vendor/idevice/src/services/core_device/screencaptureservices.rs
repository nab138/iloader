// Jackson Coxson

use crate::{IdeviceError, ReadWrite, RsdService, obf};

use super::CoreDeviceError;

/// Image encoding requested from the device for [`ScreenCaptureServiceClient::take_screenshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    /// PNG (lossless). The format Xcode's Devices window uses.
    #[default]
    Png,
    /// JPEG (lossy, smaller).
    Jpeg,
}

impl ImageFormat {
    /// The `requestedFormat` wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
        }
    }
}

impl RsdService for ScreenCaptureServiceClient<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.coredevice.screencaptureservice")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Ok(Self {
            inner: super::CoreDeviceServiceClient::new(stream).await?,
        })
    }
}

#[derive(Debug)]
pub struct ScreenCaptureServiceClient<R: ReadWrite> {
    inner: super::CoreDeviceServiceClient<R>,
}

impl<R: ReadWrite> ScreenCaptureServiceClient<R> {
    /// Capture a screenshot of `display_unique_id` (or the primary display when
    /// `None`) encoded as `format`. Returns the raw image bytes.
    pub async fn take_screenshot(
        &mut self,
        display_unique_id: Option<&str>,
        format: ImageFormat,
    ) -> Result<Vec<u8>, IdeviceError> {
        let mut req = plist::Dictionary::new();
        req.insert("requestedFormat".into(), format.as_str().into());
        if let Some(id) = display_unique_id {
            req.insert("displayUniqueID".into(), id.into());
        }

        let res = self
            .inner
            .invoke_with_plist_action(
                obf!("com.apple.coredevice.feature.capturescreenshot"),
                req,
                obf!("com.apple.coredevice.action.capturescreenshot"),
            )
            .await?;

        match res.as_dictionary().and_then(|d| d.get("image")) {
            Some(plist::Value::Data(image)) => Ok(image.clone()),
            _ => Err(CoreDeviceError::MissingField("image").into()),
        }
    }
}
