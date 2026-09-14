//! Full IPSW restore
//!
//! Reimplements the device-restore protocol stack in pure Rust:
//!
//! - [`ipsw`] - reading the IPSW archive and its `BuildManifest.plist`.
//! - [`img4`] - IMG4 personalization (stitching a component with its TSS ticket).
//! - [`restored`] - the restore-mode `com.apple.mobile.restored` client.
//! - [`options`] - the `RestoreOptions` sent with `StartRestore`.
//! - [`state_machine`] - the message loop driving the restore to completion.
//! - [`data_request`] - per-`DataType` request handlers.
//! - [`asr`] - Apple Software Restore: streaming the filesystem image.
//! - [`fdr`] - the FDR trust channel (spawned alongside the restore loop).

#[cfg(feature = "restore")]
pub mod asr;
#[cfg(feature = "restore")]
pub mod data_request;
pub mod errors;
#[cfg(feature = "restore")]
pub mod fdr;
#[cfg(feature = "restore")]
pub mod fw_updater;
#[cfg(feature = "restore")]
pub mod img4;
#[cfg(feature = "restore")]
pub mod ipsw;
#[cfg(feature = "restore")]
pub mod mbn;
#[cfg(feature = "restore")]
pub mod options;
#[cfg(feature = "restore_recovery")]
pub mod recovery;
#[cfg(feature = "restore")]
pub mod restored;
#[cfg(feature = "restore")]
pub mod state_machine;

#[cfg(feature = "restore")]
pub use asr::{AsrClient, FilesystemImage};
pub use errors::RestoreError;
#[cfg(feature = "restore")]
pub use fdr::{FdrClient, FdrConnector, run_fdr_listener};
#[cfg(feature = "restore")]
pub use options::RestoreOptions;
#[cfg(feature = "restore_recovery")]
pub use recovery::{ControlSetup, DeviceInfo, Mode, RecoveryDevice, RecoveryTransport};
#[cfg(feature = "restore")]
pub use restored::RestoredClient;
#[cfg(feature = "restore")]
pub use state_machine::{
    ComponentReader, ComponentSource, DataPortConnector, ProviderDataPorts, RestoreCancel,
    RestoreContext, RestoreProgressEvent, RestoreProgressReceiver, RestoreProgressSender,
    progress_channel, run_restore,
};
