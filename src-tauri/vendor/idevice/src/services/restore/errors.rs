//! Errors specific to the IPSW restore flow.

use thiserror::Error;

/// Failures that can occur while restoring an IPSW onto a device.
///
/// Grouped by the stage that produced them so a library consumer can react to a
/// category (e.g. retry a data-port connect, surface a device-reported error to
/// the user).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum RestoreError {
    /// A component named by the build identity could not be found in it.
    #[error("component `{0}` not found in build identity")]
    ComponentNotFound(String),

    /// Reading or extracting from the IPSW archive failed.
    #[error("IPSW archive error: {0}")]
    Ipsw(String),

    /// IMG4 personalization (IM4P/IM4M/IM4R stitching) failed.
    #[error("IMG4 personalization error: {0}")]
    Img4(String),

    /// Assembling or stitching baseband firmware failed.
    #[error("baseband firmware error: {0}")]
    Baseband(String),

    /// The device rejected the baseband update it was sent.
    #[error("device rejected the baseband update: {0}")]
    BasebandRejected(String),

    /// The Apple Software Restore filesystem transfer failed.
    #[error("ASR filesystem transfer error: {0}")]
    Asr(String),

    /// No filesystem image was supplied to a restore that needs one.
    #[error("no filesystem image was provided for the restore")]
    NoFilesystemImage,

    /// Could not open a connection to a restore-mode data port.
    #[error("could not connect to restore data port {0}")]
    DataPortConnect(u16),

    /// A recovery- or DFU-mode USB operation failed.
    #[error("recovery/DFU error: {0}")]
    Recovery(String),

    /// A preboard stashbag operation (create/commit) failed.
    #[error("preboard stashbag error: {0}")]
    Stashbag(String),

    /// A TSS response was missing or had an unexpected shape.
    #[error("malformed TSS response: {0}")]
    TssResponse(String),

    /// A message from the device was missing a field the handler required.
    #[error("device message missing required field `{0}`")]
    MissingField(String),

    /// `restored` crashed partway through the restore.
    #[error("restored crashed during the restore")]
    RestoredCrashed,

    /// The device reported a fatal, structured restore error.
    #[error("device reported a fatal restore error (AMRError={amr_error}): {detail}")]
    DeviceReported {
        /// The `AMRError` value the device sent, or `-1` when absent.
        amr_error: i64,
        /// The human-readable error detail extracted from the device message.
        detail: String,
    },

    /// A restore feature or code path the device asked for is not implemented.
    #[error("{0} is not supported")]
    Unsupported(String),

    /// The consumer requested cancellation. The restore stops at the next check
    /// point and the device is rebooted toward recovery (see [`run_restore`]).
    ///
    /// [`run_restore`]: super::run_restore
    #[error("the restore was cancelled")]
    Cancelled,

    /// Any other restore failure that doesn't fit a more specific variant.
    #[error("{0}")]
    Other(String),
}

impl RestoreError {
    /// Returns the sub-error code within the restore category, for FFI consumers.
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::ComponentNotFound(_) => 1,
            Self::Ipsw(_) => 2,
            Self::Img4(_) => 3,
            Self::Baseband(_) => 4,
            Self::BasebandRejected(_) => 5,
            Self::Asr(_) => 6,
            Self::NoFilesystemImage => 7,
            Self::DataPortConnect(_) => 8,
            Self::Recovery(_) => 9,
            Self::Stashbag(_) => 10,
            Self::TssResponse(_) => 11,
            Self::MissingField(_) => 12,
            Self::RestoredCrashed => 13,
            Self::DeviceReported { .. } => 14,
            Self::Unsupported(_) => 15,
            Self::Cancelled => 17,
            Self::Other(_) => 16,
        }
    }
}
