// Jackson Coxson

/// Errors specific to the USB multiplexer daemon protocol
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum UsbmuxdError {
    #[error("device refused connection")]
    ConnectionRefused,
    #[error("bad command")]
    BadCommand,
    #[error("bad device")]
    BadDevice,
    #[error("usb bad version")]
    BadVersion,
    #[error("request missing required field: {0}")]
    MissingField(&'static str),
    #[error("request field had an unexpected type: {0}")]
    UnexpectedFieldType(&'static str),
    #[error("unknown or unsupported MessageType: {0}")]
    UnknownMessageType(String),
}

impl UsbmuxdError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::ConnectionRefused => 1,
            Self::BadCommand => 2,
            Self::BadDevice => 3,
            Self::BadVersion => 4,
            Self::MissingField(_) => 5,
            Self::UnexpectedFieldType(_) => 6,
            Self::UnknownMessageType(_) => 7,
        }
    }
}
