//! Programmatic device rotation via the `com.apple.coredevice.devicecontrol`
//! RemoteXPC service.

use std::borrow::Cow;

use crate::{IdeviceError, ReadWrite, RemoteXpcClient, RsdService, obf, xpc};

use super::CoreDeviceError;

/// Which way to rotate the device by 90 degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationDirection {
    /// Counter-clockwise.
    Left,
    /// Clockwise.
    Right,
}

impl RotationDirection {
    /// The `rotate` wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            RotationDirection::Left => "left",
            RotationDirection::Right => "right",
        }
    }
}

/// A device orientation as reported by `devicecontrol`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
    FaceUp,
    FaceDown,
    /// A value the device reported that this crate doesn't have a variant for.
    Unknown(String),
}

impl Orientation {
    fn from_wire(s: &str) -> Self {
        match s {
            "portrait" => Orientation::Portrait,
            "portraitUpsideDown" => Orientation::PortraitUpsideDown,
            "landscapeLeft" => Orientation::LandscapeLeft,
            "landscapeRight" => Orientation::LandscapeRight,
            "faceUp" => Orientation::FaceUp,
            "faceDown" => Orientation::FaceDown,
            other => Orientation::Unknown(other.to_string()),
        }
    }
}

/// The device's orientation after a rotation request.
#[derive(Debug, Clone)]
pub struct OrientationState {
    pub orientation: Orientation,
    pub non_flat_orientation: Orientation,
    /// whether rotation lock is engaged.
    pub locked: bool,
}

#[derive(Debug)]
pub struct OrientationServiceClient<R: ReadWrite> {
    inner: RemoteXpcClient<R>,
}

impl RsdService for OrientationServiceClient<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> Cow<'static, str> {
        obf!("com.apple.coredevice.devicecontrol")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        let mut inner = RemoteXpcClient::new(stream).await?;
        inner.do_handshake().await?;
        Ok(Self { inner })
    }
}

impl<R: ReadWrite> OrientationServiceClient<R> {
    pub fn new(inner: RemoteXpcClient<R>) -> Self {
        Self { inner }
    }

    /// Rotate the device 90 degrees in `direction`, returning the device's
    /// resulting [`OrientationState`].
    pub async fn rotate(
        &mut self,
        direction: RotationDirection,
    ) -> Result<OrientationState, IdeviceError> {
        let id: Cow<str> = obf!("com.apple.coredevice.feature.remote.devicecontrol.orientation");
        let msg = xpc!({
            "featureIdentifier": id.to_string(),
            "messageType": "OrientationRequest",
            "payload": {
                "rotate": {
                    "_0": direction.as_str()
                }
            }
        });

        self.inner.send_object(msg, true).await?;
        let res = self.inner.recv().await?;

        let dict = res
            .as_dictionary()
            .ok_or(CoreDeviceError::MalformedField("(root)"))?;
        let orientation = dict
            .get("currentDeviceOrientation")
            .and_then(|v| v.as_string())
            .ok_or(CoreDeviceError::MissingField("currentDeviceOrientation"))?;
        let non_flat = dict
            .get("currentDeviceNonFlatOrientation")
            .and_then(|v| v.as_string())
            .ok_or(CoreDeviceError::MissingField(
                "currentDeviceNonFlatOrientation",
            ))?;
        let locked = dict
            .get("currentDeviceOrientationLocked")
            .and_then(|v| v.as_boolean())
            .ok_or(CoreDeviceError::MissingField(
                "currentDeviceOrientationLocked",
            ))?;

        Ok(OrientationState {
            orientation: Orientation::from_wire(orientation),
            non_flat_orientation: Orientation::from_wire(non_flat),
            locked,
        })
    }
}
