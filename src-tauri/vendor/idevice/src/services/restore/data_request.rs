//! `DataRequestMsg` handlers
//!
//! Dispatches on a data request's `DataType` and produces the reply the device
//! expects. Firmware components are personalized with the TSS ticket via
//! [`img4`](super::img4). Some replies go over the restore connection, others to
//! a per-request data port. AEA-encrypted images are decrypted on the device,
//! the host only proxies the `URLAsset`/`StreamedImageDecryptionKey` HTTP
//! requests to Apple.

use std::time::Duration;

use plist::Value;
use tracing::{debug, error, info, warn};

use super::{
    asr::AsrClient,
    img4, ipsw,
    state_machine::{RestoreCancel, RestoreContext, RestoreProgressEvent, RestoreProgressSender},
};
use crate::{Idevice, IdeviceError, services::restore::RestoreError};

const FILE_CHUNK_SIZE: usize = 8192;

pub(super) const PROGRESS_STRIDE: u64 = 8 * 1024 * 1024;

pub(super) fn is_cancelled(cancel: Option<&RestoreCancel>) -> bool {
    cancel.is_some_and(RestoreCancel::is_cancelled)
}

pub(super) fn emit_transfer(
    progress: Option<&RestoreProgressSender>,
    component: &str,
    sent: u64,
    total: Option<u64>,
) {
    if let Some(tx) = progress {
        tx.send(RestoreProgressEvent::Transfer {
            component: component.to_string(),
            sent,
            total,
        });
    }
}

/// Dispatches a `DataRequestMsg`/`AsyncDataRequestMsg` on its `DataType`.
pub(super) async fn dispatch(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    let data_type = match message.get("DataType").and_then(Value::as_string) {
        Some(d) => d.to_string(),
        None => {
            warn!("data request without a string DataType: {message:?}");
            return Ok(());
        }
    };

    ctx.emit(RestoreProgressEvent::Step(data_type.clone()));

    match data_type.as_str() {
        "RootTicket" => send_root_ticket(ctx, message).await,
        "SystemImageData" | "RecoveryOSASRImage" => send_filesystem(ctx, message).await,
        "BuildIdentityDict" => send_buildidentity(ctx, message).await,
        "PersonalizedBootObjectV3" => send_boot_object(ctx, message, false).await,
        "SourceBootObjectV4" | "SourceBootObjectV5" => send_boot_object(ctx, message, true).await,
        "KernelCache" => send_component(ctx, "KernelCache", "KernelCache").await,
        "DeviceTree" => send_component(ctx, "DeviceTree", "DeviceTree").await,
        "SystemImageRootHash" => send_component(ctx, "SystemVolume", "SystemImageRootHash").await,
        "SystemImageCanonicalMetadata" => {
            send_component(
                ctx,
                "Ap,SystemVolumeCanonicalMetadata",
                "SystemImageCanonicalMetadata",
            )
            .await
        }
        "NORData" => send_nor(ctx, message).await,
        "FirmwareUpdaterData" => super::fw_updater::send_firmware_updater_data(ctx, message).await,
        "BasebandData" => super::fw_updater::send_baseband_data(ctx, message).await,
        "FDRTrustData" => send_fdr_trust_data(ctx, message).await,
        "FUDData" => {
            send_image_data(
                ctx,
                message,
                "FUDImageList",
                Some("IsFUDFirmware"),
                "FUDImageData",
            )
            .await
        }
        "PersonalizedData" => send_image_data(ctx, message, "ImageList", None, "ImageData").await,
        "EANData" => {
            send_image_data(
                ctx,
                message,
                "EANImageList",
                Some("IsEarlyAccessFirmware"),
                "EANData",
            )
            .await
        }
        "ReceiptManifest" => send_manifest(ctx).await,
        "URLAsset" => send_url_asset(ctx, message).await,
        "StreamedImageDecryptionKey" => send_streamed_image_decryption_key(ctx, message).await,
        other => {
            warn!("DataType `{other}` not yet implemented; skipping");
            Ok(())
        }
    }
}

async fn personalize_path(
    ctx: &mut RestoreContext<'_>,
    component_name: &str,
    path: &str,
) -> Result<Vec<u8>, IdeviceError> {
    let raw = ctx.components.read_component(path).await?;
    let fourcc = img4::restore_fourcc_override(component_name);
    img4::stitch_component(&raw, ctx.tss_ticket, fourcc, &[])
}

async fn personalize_named(
    ctx: &mut RestoreContext<'_>,
    component_name: &str,
) -> Result<Vec<u8>, IdeviceError> {
    let path = ipsw::component_path(ctx.build_identity, component_name)?;
    personalize_path(ctx, component_name, &path).await
}

async fn send_root_ticket(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    if ctx.tss_ticket.is_empty() {
        return Err(IdeviceError::Restore(RestoreError::Other(
            "cannot send RootTicket without a TSS ticket".into(),
        )));
    }
    info!("sending RootTicket");
    let payload = crate::plist!({ "RootTicketData": ctx.tss_ticket.to_vec() });
    send_to_data_service(ctx, message, payload).await
}

async fn send_buildidentity(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    info!("sending BuildIdentityDict");
    let variant = message
        .get("Arguments")
        .and_then(Value::as_dictionary)
        .and_then(|a| a.get("Variant"))
        .and_then(Value::as_string)
        .unwrap_or("Erase")
        .to_string();
    let payload = crate::plist!({
        "BuildIdentityDict": Value::Dictionary(ctx.build_identity.clone()),
        "Variant": variant,
    });
    send_to_data_service(ctx, message, payload).await
}

async fn connect_data_port(
    data_ports: &dyn super::state_machine::DataPortConnector,
    port: u16,
) -> Result<Idevice, IdeviceError> {
    const ATTEMPTS: usize = 30;
    let mut last_err = None;
    for attempt in 1..=ATTEMPTS {
        match data_ports.connect(port).await {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                debug!("data port {port} connect attempt {attempt}/{ATTEMPTS} failed: {e}");
                last_err = Some(e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| IdeviceError::Restore(RestoreError::DataPortConnect(port))))
}

async fn send_filesystem(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    let port = message
        .get("DataPort")
        .and_then(Value::as_unsigned_integer)
        .map(|p| p as u16)
        .unwrap_or(AsrClient::DEFAULT_PORT);
    info!("streaming filesystem image over ASR (port {port})");

    let conn = connect_data_port(ctx.data_ports, port).await?;
    let mut asr = AsrClient::connect(conn).await?;

    let image = ctx
        .filesystem
        .take()
        .ok_or(IdeviceError::Restore(RestoreError::NoFilesystemImage))?;

    let progress = ctx.progress.clone();
    let cancel = ctx.cancel.clone();
    let mut pump_ctx = ctx.without_filesystem();

    tokio::select! {
        r = asr.send_filesystem(image, progress, cancel) => r?,
        // The pump only returns on error; the transfer completing ends the select.
        r = run_transfer_pump(&mut pump_ctx) => r?,
    }

    Ok(())
}

async fn run_transfer_pump(ctx: &mut RestoreContext<'_>) -> Result<(), IdeviceError> {
    loop {
        ctx.check_cancel()?;
        let msg = ctx.restored.recv().await?;
        match msg
            .get("MsgType")
            .and_then(Value::as_string)
            .unwrap_or_default()
        {
            "DataRequestMsg" | "AsyncDataRequestMsg" => {
                // Boxed to break the dispatch -> filesystem -> pump -> dispatch cycle.
                Box::pin(dispatch(ctx, &msg)).await.inspect_err(|e| {
                    error!("data request during transfer failed, aborting restore: {e}");
                })?;
            }
            other => debug!("message during transfer: {other}"),
        }
    }
}

async fn send_component(
    ctx: &mut RestoreContext<'_>,
    component: &str,
    reply_name: &str,
) -> Result<(), IdeviceError> {
    info!("personalizing and sending {reply_name}");
    let personalized = personalize_named(ctx, component).await?;
    let mut reply = plist::Dictionary::new();
    reply.insert(format!("{reply_name}File"), Value::Data(personalized));
    ctx.restored.send(Value::Dictionary(reply)).await
}

async fn send_boot_object(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
    handle_aea: bool,
) -> Result<(), IdeviceError> {
    let image_name = message
        .get("Arguments")
        .and_then(Value::as_dictionary)
        .and_then(|a| a.get("ImageName"))
        .and_then(Value::as_string)
        .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("ImageName".into())))?
        .to_string();
    info!("sending boot object {image_name}");

    let port = message
        .get("DataPort")
        .and_then(Value::as_unsigned_integer)
        .map(|p| p as u16);

    let special = match image_name.as_str() {
        "__RestoreVersion__" => Some("RestoreVersion.plist"),
        "__SystemVersion__" => Some("SystemVersion.plist"),
        "__GlobalManifest__" => {
            return Err(IdeviceError::Restore(RestoreError::Unsupported(
                "__GlobalManifest__ (macOS)".into(),
            )));
        }
        _ => None,
    };

    if handle_aea && special.is_none() {
        return send_source_boot_object(ctx, &image_name, port).await;
    }

    let data = match special {
        Some(path) => ctx.components.read_component(path).await?,
        None => personalize_named(ctx, &image_name).await?,
    };

    match port {
        Some(port) => {
            let mut conn = ctx.data_ports.connect(port).await?;
            send_file_chunks(&mut conn, &data, handle_aea).await
        }
        None => {
            for chunk in data.chunks(FILE_CHUNK_SIZE) {
                ctx.restored
                    .send(crate::plist!({ "FileData": chunk.to_vec() }))
                    .await?;
            }
            ctx.restored
                .send(crate::plist!({ "FileDataDone": true }))
                .await
        }
    }
}

async fn send_source_boot_object(
    ctx: &mut RestoreContext<'_>,
    image_name: &str,
    port: Option<u16>,
) -> Result<(), IdeviceError> {
    let path = ipsw::component_path(ctx.build_identity, image_name)?;

    // disassemble to not pass entire objects
    let RestoreContext {
        restored,
        components,
        data_ports,
        progress,
        cancel,
        ..
    } = ctx;

    let mut reader = components.open_component(&path).await?;
    let mut sink = match port {
        Some(port) => BootSink::Port(connect_data_port(*data_ports, port).await?),
        None => BootSink::Restored(restored),
    };
    stream_boot_object(
        image_name,
        &mut *reader,
        &mut sink,
        progress.as_ref(),
        cancel.as_ref(),
    )
    .await
}

enum BootSink<'a> {
    Port(Idevice),
    Restored(&'a mut super::restored::RestoredClient),
}

impl BootSink<'_> {
    async fn send(&mut self, value: Value) -> Result<(), IdeviceError> {
        match self {
            BootSink::Port(conn) => conn.send_plist(value).await,
            BootSink::Restored(restored) => restored.send(value).await,
        }
    }

    async fn recv(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        match self {
            BootSink::Port(conn) => conn.read_plist().await,
            BootSink::Restored(restored) => restored.recv().await,
        }
    }
}

async fn stream_boot_object(
    component: &str,
    reader: &mut dyn super::state_machine::ComponentReader,
    sink: &mut BootSink<'_>,
    progress: Option<&RestoreProgressSender>,
    cancel: Option<&RestoreCancel>,
) -> Result<(), IdeviceError> {
    let mut buf = vec![0u8; FILE_CHUNK_SIZE];
    let mut first = true;
    let mut sent: u64 = 0;
    let mut next_report: u64 = 0;
    loop {
        if is_cancelled(cancel) {
            return Err(IdeviceError::Restore(RestoreError::Cancelled));
        }
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        sink.send(crate::plist!({ "FileData": buf[..n].to_vec() }))
            .await?;
        sent += n as u64;
        if sent >= next_report {
            emit_transfer(progress, component, sent, None);
            next_report = sent + PROGRESS_STRIDE;
        }
        if first {
            first = false;
            if buf[..n].starts_with(b"AEA1")
                && let Ok(Ok(msg)) = tokio::time::timeout(Duration::from_secs(3), sink.recv()).await
                && msg.get("MsgType").and_then(Value::as_string) == Some("URLAsset")
            {
                let response = url_asset_response(&msg).await?;
                sink.send(response).await?;
            }
        }
    }
    sink.send(crate::plist!({ "FileDataDone": true })).await
}

async fn send_file_chunks(
    conn: &mut Idevice,
    data: &[u8],
    handle_aea: bool,
) -> Result<(), IdeviceError> {
    for (i, chunk) in data.chunks(FILE_CHUNK_SIZE).enumerate() {
        conn.send_plist(crate::plist!({ "FileData": chunk.to_vec() }))
            .await?;
        if handle_aea
            && i == 0
            && chunk.starts_with(b"AEA1")
            && let Ok(Ok(msg)) =
                tokio::time::timeout(Duration::from_secs(3), conn.read_plist()).await
            && msg.get("MsgType").and_then(Value::as_string) == Some("URLAsset")
        {
            let response = url_asset_response(&msg).await?;
            conn.send_plist(response).await?;
        }
    }
    conn.send_plist(crate::plist!({ "FileDataDone": true }))
        .await
}

async fn send_nor(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    info!("assembling NORData");
    let flash_version_1 = message
        .get("Arguments")
        .and_then(Value::as_dictionary)
        .and_then(|a| a.get("FlashVersion1"))
        .map(is_truthy)
        .unwrap_or(false);

    let llb_path = ipsw::component_path(ctx.build_identity, "LLB")?;
    let firmware = firmware_components(ctx.build_identity);

    let mut req = plist::Dictionary::new();
    let llb = personalize_path(ctx, "LLB", &llb_path).await?;
    req.insert("LlbImageData".into(), Value::Data(llb));

    if flash_version_1 {
        let mut d = plist::Dictionary::new();
        for (name, path) in &firmware {
            if name == "LLB" || name == "RestoreSEP" {
                continue;
            }
            let data = personalize_path(ctx, name, path).await?;
            d.insert(name.clone(), Value::Data(data));
        }
        req.insert("NorImageData".into(), Value::Dictionary(d));
    } else {
        let mut arr: Vec<Value> = Vec::new();
        for (name, path) in &firmware {
            if name == "LLB" || name == "RestoreSEP" {
                continue;
            }
            let data = Value::Data(personalize_path(ctx, name, path).await?);
            if name.starts_with("iBoot") {
                arr.insert(0, data);
            } else {
                arr.push(data);
            }
        }
        req.insert("NorImageData".into(), Value::Array(arr));
    }

    // SEP images are sent under their own keys (SepStage1 -> SEPPatch).
    for component in ["RestoreSEP", "SEP", "SepStage1"] {
        if let Ok(path) = ipsw::component_path(ctx.build_identity, component) {
            let key = if component == "SepStage1" {
                "SEPPatch"
            } else {
                component
            };
            let data = personalize_path(ctx, component, &path).await?;
            req.insert(format!("{key}ImageData"), Value::Data(data));
        }
    }

    info!("sending NORData");
    send_to_data_service(ctx, message, Value::Dictionary(req)).await
}

async fn send_fdr_trust_data(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    info!("sending FDR trust data");
    send_to_data_service(ctx, message, Value::Dictionary(plist::Dictionary::new())).await
}

async fn send_manifest(ctx: &mut RestoreContext<'_>) -> Result<(), IdeviceError> {
    let manifest = ctx
        .build_identity
        .get("Manifest")
        .cloned()
        .unwrap_or_else(|| Value::Dictionary(plist::Dictionary::new()));
    ctx.restored
        .send(crate::plist!({ "ReceiptManifest": manifest }))
        .await
}

async fn send_image_data(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
    image_list_k: &str,
    image_type_k: Option<&str>,
    image_data_k: &str,
) -> Result<(), IdeviceError> {
    let arguments = message.get("Arguments").and_then(Value::as_dictionary);
    let want_list = arguments
        .and_then(|a| a.get(image_list_k))
        .map(is_truthy)
        .unwrap_or(false);
    let mut image_name = arguments
        .and_then(|a| a.get("ImageName"))
        .and_then(Value::as_string)
        .map(str::to_string);

    // The image type key is either fixed or taken from the request.
    let type_key = match image_type_k {
        Some(k) => k.to_string(),
        None => arguments
            .and_then(|a| a.get("ImageType"))
            .and_then(Value::as_string)
            .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("ImageType".into())))?
            .to_string(),
    };

    let manifest = ctx
        .build_identity
        .get("Manifest")
        .and_then(Value::as_dictionary)
        .ok_or(IdeviceError::BadBuildManifest)?
        .clone();

    // Normalize an `Ap...` image name to `Ap,...` if needed.
    if let Some(name) = &image_name
        && !want_list
        && !manifest.contains_key(name)
        && name.starts_with("Ap")
    {
        let fixed = name.replacen("Ap", "Ap,", 1);
        if manifest.contains_key(&fixed) {
            image_name = Some(fixed);
        }
    }

    let mut matched: Vec<String> = Vec::new();
    let mut data_dict = plist::Dictionary::new();
    for (component, entry) in &manifest {
        let is_type = entry
            .as_dictionary()
            .and_then(|e| e.get("Info"))
            .and_then(Value::as_dictionary)
            .and_then(|i| i.get(&type_key))
            .map(is_truthy)
            .unwrap_or(false);
        if !is_type {
            continue;
        }
        if want_list {
            matched.push(component.clone());
        } else if image_name.as_deref().is_none_or(|n| n == component) {
            let data = personalize_named(ctx, component).await?;
            data_dict.insert(component.clone(), Value::Data(data));
        }
    }

    let mut req = plist::Dictionary::new();
    if want_list {
        req.insert(
            image_list_k.into(),
            Value::Array(matched.into_iter().map(Value::String).collect()),
        );
    } else if let Some(name) = image_name {
        if let Some(data) = data_dict.get(&name) {
            req.insert(image_data_k.into(), data.clone());
        }
        req.insert("ImageName".into(), Value::String(name));
    } else {
        req.insert(image_data_k.into(), Value::Dictionary(data_dict));
    }

    ctx.restored.send(Value::Dictionary(req)).await
}

async fn send_url_asset(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    let response = url_asset_response(message).await?;
    send_to_data_service(ctx, message, response).await
}

async fn send_streamed_image_decryption_key(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    let reply = streamed_key_response(message).await?;
    send_to_data_service(ctx, message, reply).await
}

async fn streamed_key_response(message: &plist::Dictionary) -> Result<Value, IdeviceError> {
    let arguments = message
        .get("Arguments")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("Arguments".into())))?;
    let url = arguments
        .get("RequestURL")
        .and_then(Value::as_string)
        .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("RequestURL".into())))?;
    let body = arguments
        .get("RequestBody")
        .and_then(Value::as_data)
        .unwrap_or_default()
        .to_vec();
    info!("proxying StreamedImageDecryptionKey POST {url}");

    crate::ensure_default_crypto_provider();
    let mut req = reqwest::Client::new().post(url).body(body);
    if let Some(headers) = arguments
        .get("RequestAdditionalHeaders")
        .and_then(Value::as_dictionary)
    {
        for (k, v) in headers {
            if let Some(v) = v.as_string() {
                req = req.header(k.as_str(), v);
            }
        }
    }
    http_response_to_plist(req.send().await?).await
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Boolean(b) => *b,
        Value::Integer(i) => i.as_unsigned().map(|n| n != 0).unwrap_or(true),
        Value::String(s) => !s.is_empty() && s != "0" && s != "false",
        _ => true,
    }
}

fn firmware_components(build_identity: &plist::Dictionary) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(manifest) = build_identity
        .get("Manifest")
        .and_then(Value::as_dictionary)
    else {
        return out;
    };
    for (name, entry) in manifest {
        let Some(info) = entry
            .as_dictionary()
            .and_then(|e| e.get("Info"))
            .and_then(Value::as_dictionary)
        else {
            continue;
        };
        let is_fw = info
            .get("IsFirmwarePayload")
            .map(is_truthy)
            .unwrap_or(false);
        let is_secondary = info
            .get("IsSecondaryFirmwarePayload")
            .map(is_truthy)
            .unwrap_or(false);
        let loaded_by_iboot = info.get("IsLoadedByiBoot").map(is_truthy).unwrap_or(false);
        if (is_fw || (is_secondary && loaded_by_iboot))
            && let Some(path) = info.get("Path").and_then(Value::as_string)
        {
            out.push((name.clone(), path.to_string()));
        }
    }
    out
}

async fn url_asset_response(message: &plist::Dictionary) -> Result<Value, IdeviceError> {
    let arguments = message
        .get("Arguments")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("Arguments".into())))?;
    let url = arguments
        .get("RequestURL")
        .and_then(Value::as_string)
        .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("RequestURL".into())))?;
    info!("proxying URLAsset GET {url}");
    crate::ensure_default_crypto_provider();
    let response = reqwest::Client::new().get(url).send().await?;
    http_response_to_plist(response).await
}

async fn http_response_to_plist(response: reqwest::Response) -> Result<Value, IdeviceError> {
    let status = response.status().as_u16() as i64;
    let mut headers = plist::Dictionary::new();
    for (k, v) in response.headers() {
        if let Ok(v) = v.to_str() {
            headers.insert(k.as_str().to_string(), Value::String(v.to_string()));
        }
    }
    let body = response.bytes().await?.to_vec();
    Ok(crate::plist!({
        "ResponseBody": body,
        "ResponseBodyDone": true,
        "ResponseHeaders": Value::Dictionary(headers),
        "ResponseStatus": status,
    }))
}

pub(super) async fn send_to_data_service(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
    payload: Value,
) -> Result<(), IdeviceError> {
    match message.get("DataPort").and_then(Value::as_unsigned_integer) {
        Some(port) => {
            let mut conn = ctx.data_ports.connect(port as u16).await?;
            conn.send_plist(payload).await
        }
        None => ctx.restored.send(payload).await,
    }
}
