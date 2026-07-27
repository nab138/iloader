// Jackson Coxson
// Ported from pymobiledevice3

use tracing::warn;

use crate::{
    IdeviceError, ReadWrite, RemoteXpcClient,
    xpc::{self, XPCObject},
};

mod app_service;
mod diagnosticsservice;
#[cfg(feature = "display_stream")]
pub mod display_stream;
mod errors;
#[cfg(feature = "display_stream")]
pub mod hid;
mod location_service;
mod openstdiosocket;
mod orientation_service;
mod pasteboard_service;
mod screencaptureservices;
pub use app_service::*;
pub use diagnosticsservice::*;
#[cfg(feature = "display_stream")]
pub use display_stream::*;
pub use errors::CoreDeviceError;
#[cfg(feature = "display_stream")]
pub use hid::*;
pub use location_service::*;
pub use openstdiosocket::*;
pub use orientation_service::*;
pub use pasteboard_service::*;
pub use screencaptureservices::*;

const CORE_SERVICE_VERSION: &str = "443.18";

#[derive(Debug)]
pub struct CoreDeviceServiceClient<R: ReadWrite> {
    inner: RemoteXpcClient<R>,
}

impl<R: ReadWrite> CoreDeviceServiceClient<R> {
    pub async fn new(inner: R) -> Result<Self, IdeviceError> {
        let mut client = RemoteXpcClient::new(inner).await?;
        client.do_handshake().await?;
        Ok(Self { inner: client })
    }

    pub async fn invoke_with_plist(
        &mut self,
        feature: impl Into<String>,
        input: plist::Dictionary,
    ) -> Result<plist::Value, IdeviceError> {
        let input: XPCObject = plist::Value::Dictionary(input).into();
        let input = input.to_dictionary().unwrap();
        self.invoke(feature, Some(input)).await
    }

    pub async fn invoke_with_plist_action(
        &mut self,
        feature: impl Into<String>,
        input: plist::Dictionary,
        action_identifier: impl Into<String>,
    ) -> Result<plist::Value, IdeviceError> {
        let input: XPCObject = plist::Value::Dictionary(input).into();
        let input = input.to_dictionary().unwrap();
        self.invoke_inner(feature, Some(input), Some(action_identifier.into()))
            .await
    }

    pub async fn invoke(
        &mut self,
        feature: impl Into<String>,
        input: Option<crate::xpc::Dictionary>,
    ) -> Result<plist::Value, IdeviceError> {
        self.invoke_inner(feature, input, None).await
    }

    async fn invoke_inner(
        &mut self,
        feature: impl Into<String>,
        input: Option<crate::xpc::Dictionary>,
        action_identifier: Option<String>,
    ) -> Result<plist::Value, IdeviceError> {
        let feature = feature.into();
        let input: crate::xpc::XPCObject = match input {
            Some(i) => i.into(),
            None => crate::xpc::Dictionary::new().into(),
        };

        let mut req = xpc::Dictionary::new();
        let protocol_version = if action_identifier.is_some() { 2 } else { 0 };
        req.insert(
            "CoreDevice.CoreDeviceDDIProtocolVersion".into(),
            XPCObject::Int64(protocol_version),
        );
        req.insert("CoreDevice.action".into(), xpc::Dictionary::new().into());
        req.insert(
            "CoreDevice.coreDeviceVersion".into(),
            create_xpc_version_from_string(CORE_SERVICE_VERSION).into(),
        );
        req.insert(
            "CoreDevice.deviceIdentifier".into(),
            XPCObject::String(uuid::Uuid::new_v4().to_string()),
        );
        req.insert(
            "CoreDevice.featureIdentifier".into(),
            XPCObject::String(feature),
        );
        req.insert("CoreDevice.input".into(), input);
        req.insert(
            "CoreDevice.invocationIdentifier".into(),
            XPCObject::String(uuid::Uuid::new_v4().to_string()),
        );
        if let Some(action_identifier) = action_identifier {
            req.insert(
                "CoreDevice.actionIdentifier".into(),
                XPCObject::String(action_identifier),
            );
        }

        self.inner.send_object(req, true).await?;
        let res = self.inner.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("XPC response was not a dictionary");
                return Err(CoreDeviceError::MalformedField("(root)").into());
            }
        };

        let res = match res.remove("CoreDevice.output") {
            Some(r) => r,
            None => {
                // The device replied with an error rather than an output. Surface
                // its contents (commonly under "CoreDevice.error") so callers can
                // see why a feature invocation was rejected.
                warn!("XPC response did not have an output: {res:?}");
                return match res.get("CoreDevice.error") {
                    Some(e) => Err(CoreDeviceError::DeviceError(format!("{e:?}")).into()),
                    None => Err(CoreDeviceError::MissingField("CoreDevice.output").into()),
                };
            }
        };

        Ok(res)
    }
}

fn create_xpc_version_from_string(version: impl Into<String>) -> xpc::Dictionary {
    let version: String = version.into();
    let mut collected_version = Vec::new();
    version.split('.').for_each(|x| {
        if let Ok(x) = x.parse() {
            collected_version.push(XPCObject::UInt64(x));
        }
    });

    let mut res = xpc::Dictionary::new();
    res.insert(
        "originalComponentsCount".into(),
        XPCObject::Int64(collected_version.len() as i64),
    );
    res.insert("components".into(), XPCObject::Array(collected_version));
    res.insert("stringValue".into(), XPCObject::String(version));
    res
}
