use async_zip::base::read::seek::ZipFileReader;
use async_zip::base::write::ZipFileWriter;
use async_zip::{Compression, ZipEntryBuilder};
use futures::AsyncReadExt as _;
use plist::Value;
use tracing::{info, warn};

use super::{ipsw, mbn, state_machine::RestoreContext};
use crate::{IdeviceError, services::restore::RestoreError, tss::TSSRequest};

/// Handles a `FirmwareUpdaterData` request.
pub(super) async fn send_firmware_updater_data(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    let arguments = message
        .get("Arguments")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| IdeviceError::Restore(RestoreError::MissingField("Arguments".into())))?
        .clone();

    let updater_name = arguments
        .get("MessageArgUpdaterName")
        .and_then(Value::as_string)
        .unwrap_or("<unknown>")
        .to_string();

    if arguments.get("MessageArgType").and_then(Value::as_string) != Some("FirmwareResponseData") {
        return Err(IdeviceError::Restore(RestoreError::Other(format!(
            "unexpected MessageArgType for updater {updater_name}"
        ))));
    }

    let fwdict = if arguments.get("DeviceGeneratedRequest").is_some() {
        get_device_generated_firmware_data(ctx, &arguments).await?
    } else {
        return Err(IdeviceError::Restore(RestoreError::Unsupported(format!(
            "updater `{updater_name}` without a DeviceGeneratedRequest"
        ))));
    };

    info!("sending FirmwareResponseData for {updater_name}");
    super::data_request::send_to_data_service(
        ctx,
        message,
        crate::plist!({ "FirmwareResponseData": Value::Dictionary(fwdict) }),
    )
    .await
}

/// Signs a device-generated firmware TSS request and returns the response.
async fn get_device_generated_firmware_data(
    ctx: &mut RestoreContext<'_>,
    arguments: &plist::Dictionary,
) -> Result<plist::Dictionary, IdeviceError> {
    let device_request = arguments
        .get("DeviceGeneratedRequest")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| {
            IdeviceError::Restore(RestoreError::MissingField("DeviceGeneratedRequest".into()))
        })?;

    let response_ticket = arguments
        .get("DeviceGeneratedTags")
        .and_then(Value::as_dictionary)
        .and_then(|t| t.get("ResponseTags"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_string)
        .map(str::to_string);

    let build_identity_tags: Vec<&str> = arguments
        .get("DeviceGeneratedTags")
        .and_then(Value::as_dictionary)
        .and_then(|t| t.get("BuildIdentityTags"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_string).collect())
        .unwrap_or_default();

    let mut request = TSSRequest::new();
    request.add_common_tags(ctx.board_id, ctx.chip_id, ctx.ecid, None, None);
    request.add_build_identity_tags(ctx.build_identity, &["ApBoardID", "ApChipID"]);
    request.add_build_identity_tags(ctx.build_identity, &build_identity_tags);
    request.set_bb_ticket(true);
    request.insert("ApSecurityMode", true);

    // Default production mode true, overridden by any ProductionMode in the info.
    let mut production_mode = true;
    if let Some(info) = arguments
        .get("MessageArgInfo")
        .and_then(Value::as_dictionary)
    {
        for (k, v) in info {
            if k.ends_with("ProductionMode") {
                production_mode = v.as_boolean().unwrap_or(true);
            }
        }
    }
    request.insert("ApProductionMode", production_mode);

    let manifest = ctx
        .build_identity
        .get("Manifest")
        .and_then(Value::as_dictionary);
    for (k, v) in device_request {
        match v {
            Value::Dictionary(dev_node) if dev_node.contains_key("Digest") => {
                let mut node = dev_node.clone();
                if let Some(digest) = manifest
                    .and_then(|m| m.get(k))
                    .and_then(Value::as_dictionary)
                    .and_then(|mn| mn.get("Digest"))
                {
                    node.insert("Digest".into(), digest.clone());
                }
                request.insert(k.clone(), Value::Dictionary(node));
            }
            _ => request.insert(k.clone(), v.clone()),
        }
    }

    // This redacted field must not be sent.
    request.remove("RequiresUIDMode");

    let response = request.send().await?;
    let response = match response {
        Value::Dictionary(d) => d,
        _ => {
            return Err(IdeviceError::Restore(RestoreError::TssResponse(
                "response is not a dictionary".into(),
            )));
        }
    };
    if let Some(rt) = &response_ticket
        && !response.contains_key(rt)
    {
        warn!("device-generated TSS response missing `{rt}`");
    }
    Ok(response)
}

/// Handles a `BasebandData` request: signs the baseband ticket and sends the
/// stitched baseband firmware.
pub(super) async fn send_baseband_data(
    ctx: &mut RestoreContext<'_>,
    message: &plist::Dictionary,
) -> Result<(), IdeviceError> {
    let arguments = message.get("Arguments").and_then(Value::as_dictionary);

    let bb_chip_id = arguments
        .and_then(|a| a.get("ChipID"))
        .and_then(Value::as_unsigned_integer);
    let bb_cert_id = arguments.and_then(|a| a.get("CertID")).cloned();
    let bb_snum = arguments.and_then(|a| a.get("ChipSerialNo")).cloned();
    let bb_nonce = arguments.and_then(|a| a.get("Nonce")).cloned();

    // Build the baseband TSS request.
    let mut parameters = plist::Dictionary::new();
    parameters.insert("ApECID".into(), (ctx.ecid).into());
    if let Some(n) = &bb_nonce {
        parameters.insert("BbNonce".into(), n.clone());
    }
    if let Some(id) = bb_chip_id {
        parameters.insert("BbChipID".into(), id.into());
    }
    if let Some(cert) = &bb_cert_id {
        parameters.insert("BbGoldCertId".into(), cert.clone());
    }
    if let Some(snum) = &bb_snum {
        parameters.insert("BbSNUM".into(), snum.clone());
    }
    if let Some(manifest) = ctx.build_identity.get("Manifest") {
        parameters.insert("Manifest".into(), manifest.clone());
    }

    // Copy the identity-level baseband tags the TSS request needs: the manifest
    // key hashes, the Pearl root pub, and the OS version.
    const BB_IDENTITY_KEYS: &[&str] = &[
        "BbProvisioningManifestKeyHash",
        "BbActivationManifestKeyHash",
        "BbCalibrationManifestKeyHash",
        "BbFactoryActivationManifestKeyHash",
        "BbFDRSecurityKeyHash",
        "BbSkeyId",
        "PearlCertificationRootPub",
        "Ap,OSLongVersion",
    ];
    for &key in BB_IDENTITY_KEYS {
        if let Some(v) = ctx.build_identity.get(key) {
            parameters.insert(key.into(), v.clone());
        }
    }

    let mut request = TSSRequest::new();
    request.add_common_tags(ctx.board_id, ctx.chip_id, ctx.ecid, None, None);
    request.add_baseband_tags(&parameters);
    let response = request.send().await?;
    let bbtss = match response {
        Value::Dictionary(d) => d,
        _ => {
            return Err(IdeviceError::Restore(RestoreError::TssResponse(
                "baseband response is not a dictionary".into(),
            )));
        }
    };

    // Read and stitch the baseband firmware zip.
    let bbfw_path = ipsw::component_path(ctx.build_identity, "BasebandFirmware").or_else(|_| {
        ctx.build_identity
            .get("Manifest")
            .and_then(|m| m.as_dictionary())
            .and_then(|m| m.get("BasebandFirmware"))
            .and_then(|b| b.as_dictionary())
            .and_then(|b| b.get("Info"))
            .and_then(|i| i.as_dictionary())
            .and_then(|i| i.get("Path"))
            .and_then(Value::as_string)
            .map(str::to_string)
            .ok_or_else(|| {
                IdeviceError::Restore(RestoreError::ComponentNotFound("BasebandFirmware".into()))
            })
    })?;
    let bbfw = ctx.components.read_component(&bbfw_path).await?;
    let stitched = sign_bbfw(&bbfw, &bbtss, bb_chip_id).await?;

    info!("sending BasebandData ({} bytes)", stitched.len());
    super::data_request::send_to_data_service(
        ctx,
        message,
        crate::plist!({ "BasebandData": Value::Data(stitched) }),
    )
    .await
}

/// Stitches the per-file signature blobs from `bbtss` into the baseband firmware
/// zip `bbfw`, returning a new zip.
async fn sign_bbfw(
    bbfw: &[u8],
    bbtss: &plist::Dictionary,
    bb_chip_id: Option<u64>,
) -> Result<Vec<u8>, IdeviceError> {
    let bbfw_dict = bbtss
        .get("BasebandFirmware")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Baseband(
                "BBTicket missing BasebandFirmware".into(),
            ))
        })?;

    fn zip_err<E: std::fmt::Display>(e: E) -> IdeviceError {
        IdeviceError::Restore(RestoreError::Ipsw(format!("baseband zip: {e}")))
    }

    // Read every entry of the input zip into memory (baseband firmware is small).
    let mut reader = ZipFileReader::with_tokio(std::io::Cursor::new(bbfw.to_vec()))
        .await
        .map_err(zip_err)?;
    let names: Vec<String> = reader
        .file()
        .entries()
        .iter()
        .map(|e| e.filename().as_str().unwrap_or_default().to_string())
        .collect();
    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    for (i, name) in names.iter().enumerate() {
        let mut entry = reader.reader_with_entry(i).await.map_err(zip_err)?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).await.map_err(zip_err)?;
        files.insert(name.clone(), buf);
    }

    let mut out = Vec::new();
    let mut writer = ZipFileWriter::with_tokio(&mut out);

    for (key, blob) in bbfw_dict {
        if !key.ends_with("-Blob") {
            continue;
        }
        let Value::Data(blob) = blob else { continue };
        let elem = key.split('-').next().unwrap_or(key.as_str());
        let filename = bbfw_filename_for_element(elem, bb_chip_id).ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Baseband(format!(
                "no baseband file mapping for element `{elem}`"
            )))
        })?;
        if bb_chip_id == Some(0x1F30E1) || filename.ends_with(".fls") {
            return Err(IdeviceError::Restore(RestoreError::Unsupported(format!(
                "baseband stitching for `{filename}` (Mav25/fls)"
            ))));
        }
        let data = files.get(filename).ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Baseband(format!(
                "baseband file `{filename}` missing"
            )))
        })?;
        let stitched = mbn::mbn_stitch(data, blob)?;
        files.insert(filename.to_string(), stitched);
    }

    // Keep every firmware file (`.fls/.mbn/.elf/.bin`), now stitched, and drop
    // everything else
    for (name, data) in &files {
        let keep = matches!(
            std::path::Path::new(name)
                .extension()
                .and_then(|e| e.to_str()),
            Some("fls" | "mbn" | "elf" | "bin")
        );
        if keep {
            let entry = ZipEntryBuilder::new(name.clone().into(), Compression::Stored);
            writer
                .write_entry_whole(entry, data)
                .await
                .map_err(zip_err)?;
        }
    }

    // Inject the personalization ticket. For non-fls basebands the updater reads
    // the signed ticket from `bbticket.der`.
    if let Some(Value::Data(ticket)) = bbtss.get("BBTicket") {
        let entry = ZipEntryBuilder::new("bbticket.der".into(), Compression::Stored);
        writer
            .write_entry_whole(entry, ticket)
            .await
            .map_err(zip_err)?;
    } else {
        tracing::warn!("baseband TSS response has no BBTicket; image will be unpersonalized");
    }

    writer.close().await.map_err(zip_err)?;
    Ok(out)
}

/// Maps a baseband ticket element name (e.g. `PSI`) to its firmware file name.
fn bbfw_filename_for_element(elem: &str, bb_chip_id: Option<u64>) -> Option<&'static str> {
    if bb_chip_id == Some(0x1F30E1) {
        // Mav25 (Qualcomm Snapdragon X80).
        return Some(match elem {
            "Misc" => "multi_image.mbn",
            "RestoreSBL1" => "restorexbl_sc.elf",
            "SBL1" => "xbl_sc.elf",
            "TME" => "signed_firmware_soc_view.elf",
            _ => return None,
        });
    }
    Some(match elem {
        "RamPSI" => "psi_ram.fls",
        "FlashPSI" => "psi_flash.fls",
        "eDBL" => "dbl.mbn",
        "RestoreDBL" => "restoredbl.mbn",
        "DBL" => "dbl.mbn",
        "ENANDPRG" => "ENPRG.mbn",
        "RestoreSBL1" => "restoresbl1.mbn",
        "SBL1" => "sbl1.mbn",
        "RestorePSI" => "restorepsi.bin",
        "PSI" => "psi_ram.bin",
        "RestorePSI2" => "restorepsi2.bin",
        "PSI2" => "psi_ram2.bin",
        "Misc" => "multi_image.mbn",
        _ => return None,
    })
}
