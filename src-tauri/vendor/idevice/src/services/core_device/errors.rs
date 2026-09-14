//! Errors specific to the CoreDevice services.

use thiserror::Error;

/// Failures specific to talking to the device's CoreDevice services.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum CoreDeviceError {
    /// The device returned an error envelope instead of the expected output
    /// (typically a populated `CoreDevice.error` in the response). The string is
    /// the device's own error detail.
    #[error("device returned an error: {0}")]
    DeviceError(String),

    /// A field the response was required to contain was absent.
    #[error("device response missing required field `{0}`")]
    MissingField(&'static str),

    /// A field was present but had a type or shape we couldn't interpret.
    #[error("device response field `{0}` had an unexpected type or shape")]
    MalformedField(&'static str),

    /// An AVConference media negotiation blob (offer/answer) failed to encode or
    /// decode.
    #[error("media negotiation blob error: {0}")]
    Negotiation(String),
}

impl CoreDeviceError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::DeviceError(_) => 1,
            Self::MissingField(_) => 2,
            Self::MalformedField(_) => 3,
            Self::Negotiation(_) => 4,
        }
    }
}
