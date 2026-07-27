//! Restore-mode state machine
//!
//! Once [`RestoredClient::start_restore`](super::restored::RestoredClient::start_restore)
//! kicks off the restore, the device drives the process by sending a stream of
//! messages. [`run_restore`] reads each message, dispatches on its `MsgType`
//! (and, for data requests, its `DataType`), and replies with the data the
//! device asks for until it reports final success.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{
    StreamExt,
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
};
use plist::Value;
use tracing::{debug, error, info, warn};

use super::restored::RestoredClient;
use crate::{Idevice, IdeviceError, services::restore::RestoreError};

/// Supplies firmware component bytes by their archive path.
///
/// Backed by [`Ipsw`](super::ipsw::Ipsw) in production; abstracted so the state
/// machine can be exercised without a real archive.
pub trait ComponentSource: Send {
    /// Reads the whole component at `path` into memory.
    fn read_component<'a>(
        &'a mut self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, IdeviceError>> + Send + 'a>>;

    /// Opens a streaming reader over the component at `path`.
    #[allow(clippy::type_complexity)]
    fn open_component<'a>(
        &'a mut self,
        path: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Box<dyn ComponentReader + Send + 'a>, IdeviceError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let data = self.read_component(path).await?;
            Ok(Box::new(BufferedComponentReader { data, pos: 0 })
                as Box<dyn ComponentReader + Send>)
        })
    }
}

/// A pull-based, sequential reader over a single component's bytes, yielded by
/// [`ComponentSource::open_component`].
///
/// The source is supplied by the consumer, so the library streams large boot
/// objects without assuming a host filesystem.
pub trait ComponentReader: Send {
    /// Reads the next chunk into `buf`, returning the number of bytes read
    /// (`0` marks end of stream). May return short reads.
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, IdeviceError>> + Send + 'a>>;
}

/// The default [`ComponentReader`]: hands out an already-buffered component.
struct BufferedComponentReader {
    data: Vec<u8>,
    pos: usize,
}

impl ComponentReader for BufferedComponentReader {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            let n = (self.data.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        })
    }
}

impl<R> ComponentSource for super::ipsw::Ipsw<R>
where
    R: tokio::io::AsyncBufRead + tokio::io::AsyncSeek + Unpin + Send,
{
    fn read_component<'a>(
        &'a mut self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, IdeviceError>> + Send + 'a>> {
        Box::pin(async move { self.read_file(path).await })
    }

    #[allow(clippy::type_complexity)]
    fn open_component<'a>(
        &'a mut self,
        path: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Box<dyn ComponentReader + Send + 'a>, IdeviceError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { self.open_entry_reader(path).await })
    }
}

/// Opens fresh connections to restore-mode data ports.
///
/// Some data requests carry a `DataPort` on which the reply must be sent over a
/// new connection; this abstracts establishing it.
///
/// Uses `&self` so it can be shared across the concurrent tasks a restore runs
/// (e.g. streaming the filesystem while servicing the device's async key requests).
pub trait DataPortConnector: Send + Sync {
    /// Connects to `port` on the restore-mode device.
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>>;
}

/// A [`DataPortConnector`] backed by an [`IdeviceProvider`](crate::provider::IdeviceProvider).
#[derive(Debug)]
pub struct ProviderDataPorts<'p> {
    /// The provider used to open new connections to data ports.
    pub provider: &'p dyn crate::provider::IdeviceProvider,
}

impl DataPortConnector for ProviderDataPorts<'_> {
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>> {
        self.provider.connect(port)
    }
}

/// A progress event emitted while a restore runs.
///
/// The enum derives [`serde::Serialize`] so it can cross an FFI or wasm boundary.
#[derive(Debug, Clone, serde::Serialize)]
pub enum RestoreProgressEvent {
    /// The device's own restore progress, from its `ProgressMsg`: an operation
    /// code and its completion percentage (`0`-`100`). This drives the main bar.
    Operation {
        /// The device's operation code.
        operation: u64,
        /// The operation's completion percentage (`0`-`100`).
        progress: u64,
    },
    /// A new host step began, named by the `DataType` the device requested.
    Step(String),
    /// Byte progress while streaming a large image (the filesystem over ASR, or a
    /// source boot object such as `Cryptex1,SystemOS`). `total` is `None` when the
    /// size isn't known ahead of time. Emitted at a throttled cadence, not per chunk.
    Transfer {
        /// The component being streamed.
        component: String,
        /// Bytes sent so far.
        sent: u64,
        /// Total size in bytes, when known.
        total: Option<u64>,
    },
}

/// A cooperative-cancellation handle for a running restore.
///
/// Cheap to clone and safe to use from any thread. Hold one, pass a clone as
/// [`RestoreContext::cancel`], and call [`cancel`](Self::cancel) to stop the
/// restore: it halts at the next check point (between device messages, and per
/// chunk while streaming) and [`run_restore`] reboots the device toward recovery,
/// returning [`RestoreError::Cancelled`].
#[derive(Debug, Clone, Default)]
pub struct RestoreCancel {
    flag: Arc<AtomicBool>,
}

impl RestoreCancel {
    /// Creates a fresh handle that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// The sending half of a restore progress channel, held by
/// [`RestoreContext::progress`]. Created by [`progress_channel`].
#[derive(Debug, Clone)]
pub struct RestoreProgressSender {
    tx: UnboundedSender<RestoreProgressEvent>,
}

impl RestoreProgressSender {
    /// Best-effort emit of one event; a dropped receiver is ignored.
    pub(super) fn send(&self, event: RestoreProgressEvent) {
        let _ = self.tx.unbounded_send(event);
    }
}

/// The receiving half of a restore progress channel. Await [`recv`](Self::recv)
/// until it returns `None`, which happens once the restore ends and drops the
/// sender. Created by [`progress_channel`].
#[derive(Debug)]
pub struct RestoreProgressReceiver {
    rx: UnboundedReceiver<RestoreProgressEvent>,
}

impl RestoreProgressReceiver {
    /// Waits for the next progress event, or returns `None` once the restore has
    /// finished and every sender has been dropped.
    pub async fn recv(&mut self) -> Option<RestoreProgressEvent> {
        self.rx.next().await
    }
}

/// Creates a progress channel for a restore.
///
/// Pass the returned [`RestoreProgressSender`] as [`RestoreContext::progress`] and
/// drive a UI by polling the [`RestoreProgressReceiver`] with
/// [`RestoreProgressReceiver::recv`]. This lets a consumer receive progress without
/// depending on the underlying channel crate.
pub fn progress_channel() -> (RestoreProgressSender, RestoreProgressReceiver) {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    (RestoreProgressSender { tx }, RestoreProgressReceiver { rx })
}

/// All the state and resources a running restore needs.
///
/// Every I/O surface is a consumer-supplied trait object, so the library acts as
/// the protocol state machine and the caller brings their own transports.
pub struct RestoreContext<'a> {
    /// The restore-mode control connection.
    pub restored: &'a mut RestoredClient,
    /// The selected build identity from the IPSW's `BuildManifest`.
    pub build_identity: &'a plist::Dictionary,
    /// The device's board ID.
    pub board_id: u64,
    /// The device's chip ID.
    pub chip_id: u64,
    /// The device's ECID.
    pub ecid: u64,
    /// The `ApImg4Ticket` (IM4M) obtained from TSS.
    pub tss_ticket: &'a [u8],
    /// Source of firmware component bytes.
    pub components: &'a mut dyn ComponentSource,
    /// The filesystem image (DMG) for ASR, when available.
    pub filesystem: Option<&'a mut dyn super::asr::FilesystemImage>,
    /// Factory for data-port connections.
    pub data_ports: &'a mut dyn DataPortConnector,
    /// Optional sink for [`RestoreProgressEvent`]s, for driving a UI. Obtain one
    /// from [`progress_channel`]; it is cloned into each concurrent task so every
    /// stage (including the ASR transfer) can emit. Omit it (`None`) to run silently.
    pub progress: Option<RestoreProgressSender>,
    /// Optional cooperative-cancellation handle. When the consumer calls
    /// [`RestoreCancel::cancel`], the restore stops at the next check point (between
    /// messages, and per chunk while streaming) and [`run_restore`] reboots the
    /// device toward recovery, returning [`RestoreError::Cancelled`].
    pub cancel: Option<RestoreCancel>,
}

impl std::fmt::Debug for RestoreContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreContext")
            .field("tss_ticket_len", &self.tss_ticket.len())
            .finish_non_exhaustive()
    }
}

impl RestoreContext<'_> {
    /// Reborrows this context without its filesystem image.
    ///
    /// The ASR transfer takes the filesystem while the request pump needs the
    /// rest of the resources to service the device's concurrent async requests;
    /// this hands the pump a shorter-lived context over the disjoint fields. The
    /// progress sink and cancel handle are cheap to clone, so both halves share the
    /// same consumer and the same cancel signal.
    pub(super) fn without_filesystem(&mut self) -> RestoreContext<'_> {
        RestoreContext {
            restored: &mut *self.restored,
            build_identity: self.build_identity,
            board_id: self.board_id,
            chip_id: self.chip_id,
            ecid: self.ecid,
            tss_ticket: self.tss_ticket,
            components: &mut *self.components,
            filesystem: None,
            data_ports: &mut *self.data_ports,
            progress: self.progress.clone(),
            cancel: self.cancel.clone(),
        }
    }

    /// Emits a progress event if a sink is attached. Best-effort: a full or
    /// dropped channel is ignored, so progress never blocks or fails the restore.
    pub(super) fn emit(&self, event: RestoreProgressEvent) {
        if let Some(tx) = &self.progress {
            tx.send(event);
        }
    }

    /// Whether the consumer has requested cancellation.
    pub(super) fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(RestoreCancel::is_cancelled)
    }

    /// Returns [`RestoreError::Cancelled`] if cancellation has been requested.
    pub(super) fn check_cancel(&self) -> Result<(), IdeviceError> {
        if self.is_cancelled() {
            Err(IdeviceError::Restore(RestoreError::Cancelled))
        } else {
            Ok(())
        }
    }
}

/// Drives the restore to completion.
///
/// Sends `StartRestore` with `options`, then loops reading messages from the
/// device and dispatching them until a final `StatusMsg` (`Status == 0`) arrives,
/// at which point `ReceivedFinalStatusMsg` is acknowledged and the function
/// returns `Ok(())`.
///
/// # Cancellation
/// If [`RestoreContext::cancel`] is set to `true`, the restore stops at the next
/// check point between device messages, and per chunk while streaming the
/// filesystem or a boot object.
///
/// On any exit, success, error, or cancellation, a best-effort `Goodbye` is sent
/// so the connection closes cleanly instead of being dropped mid-stream.
///
/// # Errors
/// Returns [`IdeviceError`] if the transport fails, the device reports an
/// unrecoverable condition (`RestoredCrash`), or the restore was cancelled.
pub async fn run_restore(
    mut ctx: RestoreContext<'_>,
    options: plist::Dictionary,
) -> Result<(), IdeviceError> {
    ctx.restored.start_restore(options).await?;

    let result = drive_restore(&mut ctx).await;

    if matches!(&result, Err(IdeviceError::Restore(RestoreError::Cancelled))) {
        info!("restore cancelled; rebooting device toward recovery");
        if let Err(e) = ctx.restored.reboot().await {
            debug!("reboot request during cancel failed (expected if link dropped): {e}");
        }
    }
    if let Err(e) = ctx.restored.goodbye().await {
        debug!("goodbye during teardown failed (expected if link dropped): {e}");
    }

    result
}

async fn drive_restore(ctx: &mut RestoreContext<'_>) -> Result<(), IdeviceError> {
    loop {
        // Catch a cancellation requested while we were between messages. The long
        // streaming phases poll the same flag per chunk (see `data_request`/`asr`).
        ctx.check_cancel()?;

        let message = ctx.restored.recv().await?;
        let msg_type = message
            .get("MsgType")
            .and_then(Value::as_string)
            .unwrap_or_default();

        match msg_type {
            "DataRequestMsg" | "AsyncDataRequestMsg" => {
                super::data_request::dispatch(ctx, &message)
                    .await
                    .inspect_err(|e| {
                        error!("data request handler failed, aborting restore: {e}")
                    })?;
            }
            "ProgressMsg" => handle_progress(ctx, &message),
            "StatusMsg" => {
                if handle_status(ctx, &message).await? {
                    info!("restore finished successfully");
                    return Ok(());
                }
            }
            "CheckpointMsg" => debug!("checkpoint: {message:?}"),
            "PreviousRestoreLogMsg" => debug!("previous restore log received"),
            "BBUpdateStatusMsg" => handle_bb_update_status(&message)?,
            "BasebandUpdaterOutputData" => {
                debug!("baseband updater output: {message:?}")
            }
            "RestoredCrash" => {
                error!("restored crashed: {message:?}");
                return Err(IdeviceError::Restore(RestoreError::RestoredCrashed));
            }
            "AsyncWait" => debug!("async wait: {message:?}"),
            "RestoreAttestation" => {
                debug!("restore attestation request; declining");
                ctx.restored
                    .send(crate::plist!({ "RestoreShouldAttest": false }))
                    .await?;
            }
            other => warn!("unhandled MsgType `{other}`: {message:?}"),
        }
    }
}

fn handle_progress(ctx: &RestoreContext<'_>, message: &plist::Dictionary) {
    let op = message
        .get("Operation")
        .and_then(Value::as_unsigned_integer);
    let progress = message.get("Progress").and_then(Value::as_unsigned_integer);
    debug!("progress: operation={op:?} progress={progress:?}");
    if let (Some(op), Some(progress)) = (op, progress) {
        ctx.emit(RestoreProgressEvent::Operation {
            operation: op,
            progress,
        });
    }
}

fn handle_bb_update_status(message: &plist::Dictionary) -> Result<(), IdeviceError> {
    let accepted = message
        .get("Accepted")
        .and_then(Value::as_boolean)
        .unwrap_or(false);
    if !accepted {
        let detail = message
            .get("Error")
            .and_then(Value::as_dictionary)
            .and_then(|e| {
                e.get("NSLocalizedDescription")
                    .or_else(|| e.get("NSDescription"))
            })
            .and_then(Value::as_string)
            .unwrap_or("device did not accept BasebandData");
        error!("baseband update rejected: {detail}");
        return Err(IdeviceError::Restore(RestoreError::BasebandRejected(
            detail.to_string(),
        )));
    }

    let done = message
        .get("Output")
        .and_then(Value::as_dictionary)
        .and_then(|o| o.get("done"))
        .and_then(Value::as_boolean)
        .unwrap_or(false);
    if done {
        info!("baseband update completed");
    } else {
        debug!("baseband update in progress");
    }
    Ok(())
}

async fn handle_status(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<bool, IdeviceError> {
    let status = message.get("Status").and_then(Value::as_signed_integer);
    let amr_error = message.get("AMRError").and_then(Value::as_signed_integer);
    if let Some(log) = message.get("Log").and_then(Value::as_string) {
        debug!("device log:\n{log}");
    }

    // Success: a final status of 0 with no error.
    if status == Some(0) && amr_error.unwrap_or(0) == 0 {
        ctx.restored
            .send(crate::plist!({ "MsgType": "ReceivedFinalStatusMsg" }))
            .await?;
        return Ok(true);
    }

    // A structured `Error` (or non-zero AMRError) is a fatal device-side failure.
    if message.contains_key("Error") || amr_error.unwrap_or(0) != 0 {
        let detail = message
            .get("Error")
            .and_then(collect_error_descriptions)
            .unwrap_or_else(|| "no description".into());
        error!("device reported a fatal restore error (AMRError={amr_error:?}): {detail}");
        return Err(IdeviceError::Restore(RestoreError::DeviceReported {
            amr_error: amr_error.unwrap_or(-1),
            detail,
        }));
    }

    // A non-zero status without a structured error is informational; keep going.
    if let Some(s) = status {
        error!("device reported restore status {s}");
    }
    Ok(false)
}

fn collect_error_descriptions(value: &Value) -> Option<String> {
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Dictionary(d) => {
                for (k, v) in d {
                    if k == "NSDescription" {
                        // The description is `{ EncodedObjectType, Content: <string> }`.
                        if let Some(s) = v
                            .as_dictionary()
                            .and_then(|c| c.get("Content"))
                            .and_then(Value::as_string)
                            .or_else(|| v.as_string())
                        {
                            out.push(s.trim().to_string());
                        }
                    }
                    walk(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, &mut out);
    if out.is_empty() {
        None
    } else {
        Some(out.join(" -> "))
    }
}
