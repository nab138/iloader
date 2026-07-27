//! Recovery / DFU protocol
//!
//! Recovery and DFU modes speak over raw USB rather than usbmux. idevice has
//! a "bring your own transport" philosophy, so the USB layer is a
//! consumer-implemented [`RecoveryTransport`] trait (backed by `nusb`, `rusb`,
//! whatever) and this module implements the iBoot/DFU protocol on top of it:
//! command sending, environment get/set, and firmware upload (bulk in recovery
//! mode, chunked control transfers with a trailing CRC in DFU mode).
//!
//! [`mode`] parses the USB serial string into device identifiers; [`dfu`] holds
//! the DFU upload state machine.

pub mod dfu;
pub mod mode;

use std::{future::Future, pin::Pin};

use tracing::debug;

pub use mode::{DeviceInfo, Mode};

use crate::{IdeviceError, services::restore::RestoreError};

/// Default USB transfer timeout (milliseconds).
pub const USB_TIMEOUT_MS: u32 = 10_000;
/// Bulk transfer chunk size in recovery mode.
pub const TRANSFER_SIZE_RECOVERY: usize = 0x8000;
/// Control transfer chunk size in DFU/WTF mode.
pub const TRANSFER_SIZE_DFU: usize = 0x800;

/// A boxed, `Send` future returned by [`RecoveryTransport`] methods.
pub type RecoveryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, IdeviceError>> + Send + 'a>>;

/// The parameters of a USB control transfer setup packet.
#[derive(Debug, Clone, Copy)]
pub struct ControlSetup {
    /// `bmRequestType`.
    pub request_type: u8,
    /// `bRequest`.
    pub request: u8,
    /// `wValue`.
    pub value: u16,
    /// `wIndex`.
    pub index: u16,
}

impl ControlSetup {
    /// Convenience constructor.
    pub const fn new(request_type: u8, request: u8, value: u16, index: u16) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
        }
    }
}

/// The raw USB surface required to drive a device in recovery/DFU mode.
///
/// Implemented by the caller over their chosen USB backend. Implementors target
/// the Apple device (VID `0x05AC`) already opened in a recovery/DFU mode.
///
/// `serial_number` must return the raw iBoot/DFU USB serial-number string (e.g.
/// `"CPID:8010 ... ECID:000... SRTG:[iBoot-...] NONC:... "`).
pub trait RecoveryTransport: Send + Sync + std::fmt::Debug {
    /// Host to device control transfer. Returns the number of bytes sent.
    fn control_out<'a>(
        &'a mut self,
        setup: ControlSetup,
        data: &'a [u8],
        timeout_ms: u32,
    ) -> RecoveryFuture<'a, usize>;

    /// Device to host control transfer, reading up to `length` bytes.
    fn control_in<'a>(
        &'a mut self,
        setup: ControlSetup,
        length: u16,
        timeout_ms: u32,
    ) -> RecoveryFuture<'a, Vec<u8>>;

    /// Bulk OUT transfer on `endpoint` (used for recovery-mode uploads).
    fn bulk_out<'a>(
        &'a mut self,
        endpoint: u8,
        data: &'a [u8],
        timeout_ms: u32,
    ) -> RecoveryFuture<'a, usize>;

    /// The USB serial-number string (source of mode/ECID/CPID/BDID/SRTG/nonces).
    fn serial_number(&mut self) -> RecoveryFuture<'_, String>;

    /// `idProduct` from the device descriptor (used to determine the mode).
    fn product_id(&self) -> u16;

    /// Selects the given configuration value.
    fn set_configuration(&mut self, configuration: u8) -> RecoveryFuture<'_, ()>;

    /// Claims an interface / alternate setting.
    fn claim_interface(&mut self, interface: u8, alt_setting: u8) -> RecoveryFuture<'_, ()>;

    /// Resets the USB device (the device re-enumerates afterwards).
    fn reset(&mut self) -> RecoveryFuture<'_, ()>;
}

/// A device in recovery or DFU mode, driven over a [`RecoveryTransport`].
#[derive(Debug)]
pub struct RecoveryDevice {
    transport: Box<dyn RecoveryTransport>,
    mode: Mode,
    info: DeviceInfo,
}

impl RecoveryDevice {
    /// Opens a recovery/DFU device: reads its descriptors, parses the serial
    /// string, and configures the USB interfaces.
    pub async fn new(mut transport: Box<dyn RecoveryTransport>) -> Result<Self, IdeviceError> {
        let mode = Mode::from_product_id(transport.product_id()).ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Recovery(format!(
                "not an Apple recovery/DFU product id: {:#06x}",
                transport.product_id()
            )))
        })?;
        let serial = transport.serial_number().await?;
        let info = DeviceInfo::parse(&serial);
        debug!("recovery device: mode={mode:?} info={info:?}");

        let mut dev = Self {
            transport,
            mode,
            info,
        };
        dev.configure().await?;
        Ok(dev)
    }

    /// Applies the per-mode configuration/interface setup.
    async fn configure(&mut self) -> Result<(), IdeviceError> {
        self.transport.set_configuration(1).await?;
        self.transport.claim_interface(0, 0).await?;
        if self.mode.is_recovery() && self.mode.product_id() > Mode::Recovery2.product_id() {
            self.transport.claim_interface(1, 1).await?;
        }
        Ok(())
    }

    /// The device's current mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The parsed device identifiers.
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Sends an iBoot command (`bmRequestType=0x40`), NUL-terminated, using the
    /// given `bRequest` (0 for most commands, 1 for `go`/`bootx`).
    pub async fn send_command_with_request(
        &mut self,
        command: &str,
        b_request: u8,
    ) -> Result<(), IdeviceError> {
        debug!("recovery command (req {b_request}): {command}");
        let mut data = command.as_bytes().to_vec();
        data.push(0);
        self.transport
            .control_out(
                ControlSetup::new(0x40, b_request, 0, 0),
                &data,
                USB_TIMEOUT_MS,
            )
            .await?;
        Ok(())
    }

    /// Sends an iBoot command (`bmRequestType=0x40`, `bRequest=0`), NUL-terminated.
    pub async fn send_command(&mut self, command: &str) -> Result<(), IdeviceError> {
        self.send_command_with_request(command, 0).await
    }

    /// Issues the DFU-style `bmRequestType=0x21, bRequest=1` zero-length control
    /// transfer iBoot expects after certain uploads/commands.
    pub async fn finish_transfer(&mut self) -> Result<(), IdeviceError> {
        self.transport
            .control_out(ControlSetup::new(0x21, 1, 0, 0), &[], USB_TIMEOUT_MS)
            .await?;
        Ok(())
    }

    /// Reads an environment variable via `getenv`.
    pub async fn getenv(&mut self, name: &str) -> Result<Vec<u8>, IdeviceError> {
        self.send_command(&format!("getenv {name}")).await?;
        self.transport
            .control_in(ControlSetup::new(0xC0, 0, 0, 0), 255, USB_TIMEOUT_MS)
            .await
    }

    /// Sets an environment variable via `setenv`.
    pub async fn setenv(&mut self, name: &str, value: &str) -> Result<(), IdeviceError> {
        self.send_command(&format!("setenv {name} {value}")).await
    }

    /// Enables or disables auto-boot and persists it (`saveenv`).
    pub async fn set_autoboot(&mut self, enable: bool) -> Result<(), IdeviceError> {
        self.setenv("auto-boot", if enable { "true" } else { "false" })
            .await?;
        self.send_command("saveenv").await
    }

    /// Reboots the device.
    pub async fn reboot(&mut self) -> Result<(), IdeviceError> {
        self.send_command("reboot").await
    }

    /// Uploads a firmware image to the device, choosing the transfer discipline
    /// by mode (bulk for recovery, chunked control transfers + CRC for DFU).
    pub async fn send_buffer(&mut self, buf: &[u8]) -> Result<(), IdeviceError> {
        if self.mode.is_recovery() {
            self.send_buffer_recovery(buf).await
        } else {
            dfu::send_buffer_dfu(self, buf).await
        }
    }

    /// Recovery-mode upload: initiate, then bulk-write chunks on endpoint 0x04.
    async fn send_buffer_recovery(&mut self, buf: &[u8]) -> Result<(), IdeviceError> {
        // Initiate the transfer.
        self.transport
            .control_out(ControlSetup::new(0x41, 0, 0, 0), &[], USB_TIMEOUT_MS)
            .await?;

        for chunk in buf.chunks(TRANSFER_SIZE_RECOVERY) {
            let n = self.transport.bulk_out(0x04, chunk, USB_TIMEOUT_MS).await?;
            if n != chunk.len() {
                return Err(IdeviceError::Restore(RestoreError::Recovery(format!(
                    "recovery upload short write: {n} of {}",
                    chunk.len()
                ))));
            }
        }
        Ok(())
    }

    /// Access to the transport (for the DFU module).
    pub(crate) fn transport(&mut self) -> &mut dyn RecoveryTransport {
        &mut *self.transport
    }

    /// Reads the one-byte DFU state via `GETSTATUS` (`bmRequestType=0xA1`,
    /// `bRequest=3`), returning `bStatus[4]`.
    pub(crate) async fn dfu_status(&mut self) -> Result<u8, IdeviceError> {
        let resp = self
            .transport
            .control_in(ControlSetup::new(0xA1, 3, 0, 0), 6, USB_TIMEOUT_MS)
            .await?;
        resp.get(4).copied().ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Recovery(
                "short DFU GETSTATUS response".into(),
            ))
        })
    }
}
