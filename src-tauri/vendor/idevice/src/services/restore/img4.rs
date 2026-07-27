//! IMG4 personalization (stitching)
//!
//! An IPSW firmware component is stored as an `IM4P` (payload) DER structure and
//! the TSS server returns an `IM4M` manifest (the `ApImg4Ticket`). To make a
//! component the device will accept, the two are combined, along with an
//! optional `IM4R` restore-info element into an outer `IMG4` container:
//!
//! ```text
//! IMG4 ::= SEQUENCE {
//!     "IMG4"  (IA5String),
//!     im4p    IM4P,             -- embedded verbatim from the IPSW
//!     im4m    [0] EXPLICIT,     -- the ApImg4Ticket, embedded verbatim
//!     im4r    [1] EXPLICIT      -- optional restore info (nonce slot / TBM)
//! }
//! ```
//!
//! Both the `IM4P` and `IM4M` are already valid DER produced by Apple, so they
//! are embedded byte-for-byte; only the outer container, the (optional) fourcc
//! patch and the `IM4R` element are constructed here.

use der::{Encode, Length};

use crate::{IdeviceError, services::restore::RestoreError};

// ASN.1 identifier octets used when assembling the container.
const TAG_BOOLEAN: u8 = 0x01;
const TAG_INTEGER: u8 = 0x02;
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_IA5_STRING: u8 = 0x16;
const TAG_SEQUENCE: u8 = 0x30; // SEQUENCE | CONSTRUCTED
const TAG_SET: u8 = 0x31; // SET | CONSTRUCTED
const TAG_CONTEXT_0: u8 = 0xA0; // [0] CONSTRUCTED
const TAG_CONTEXT_1: u8 = 0xA1; // [1] CONSTRUCTED
// PRIVATE | CONSTRUCTED | high-tag-number form (0xC0 | 0x20 | 0x1F).
const TAG_PRIVATE_HIGH: u8 = 0xFF;

/// A property placed in the `IM4R` restore-info element.
///
/// Each property is keyed by a four-character code (e.g. `snid`, `anid`, `ucon`,
/// `ucer`) and carried under a private-class ASN.1 tag whose number is the
/// big-endian fourcc.
#[derive(Debug, Clone)]
pub enum RestoreProperty {
    /// An integer-valued property (nonce slot IDs `snid`/`anid`).
    Integer { fourcc: [u8; 4], value: u64 },
    /// An octet-string-valued property (`*-TBM` `ucon`/`ucer` blobs).
    OctetString { fourcc: [u8; 4], value: Vec<u8> },
}

/// Encodes an ASN.1 DER length field for `len` bytes using [`der::Length`].
fn encode_len(len: usize) -> Result<Vec<u8>, IdeviceError> {
    Length::try_from(len)
        .and_then(|l| l.to_der())
        .map_err(|e| IdeviceError::Restore(RestoreError::Img4(format!("length {len}: {e}"))))
}

/// Builds a tag-length-value element with the given identifier octet and content.
fn tlv(tag: u8, content: &[u8]) -> Result<Vec<u8>, IdeviceError> {
    let mut out = Vec::with_capacity(content.len() + 4);
    out.push(tag);
    out.extend_from_slice(&encode_len(content.len())?);
    out.extend_from_slice(content);
    Ok(out)
}

/// Encodes a non-negative integer as DER INTEGER content octets (minimal length,
/// with a leading `0x00` when the most-significant bit would otherwise be set).
fn der_uint(value: u64) -> Vec<u8> {
    if value == 0 {
        return vec![0];
    }
    let mut bytes = value.to_be_bytes().to_vec();
    while bytes.len() > 1 && bytes[0] == 0 {
        bytes.remove(0);
    }
    if bytes[0] & 0x80 != 0 {
        bytes.insert(0, 0);
    }
    bytes
}

/// Encodes `value` as an ASN.1 high-tag-number (base-128) tag number body.
fn base128(mut value: u32) -> Vec<u8> {
    let mut groups = 0;
    let mut tmp = value;
    while tmp > 0 {
        tmp >>= 7;
        groups += 1;
    }
    if groups == 0 {
        groups = 1;
    }
    let mut out = vec![0u8; groups];
    for i in (0..groups).rev() {
        out[i] = (value & 0x7f) as u8;
        if i != groups - 1 {
            out[i] |= 0x80;
        }
        value >>= 7;
    }
    out
}

/// Builds one `IM4R` property: a private-tagged element wrapping a
/// `SEQUENCE { IA5String fourcc, <value> }`.
fn build_property(prop: &RestoreProperty) -> Result<Vec<u8>, IdeviceError> {
    let (fourcc, mut inner) = match prop {
        RestoreProperty::Integer { fourcc, value } => {
            let mut inner = tlv(TAG_IA5_STRING, fourcc)?;
            inner.extend_from_slice(&tlv(TAG_INTEGER, &der_uint(*value))?);
            (*fourcc, inner)
        }
        RestoreProperty::OctetString { fourcc, value } => {
            let mut inner = tlv(TAG_IA5_STRING, fourcc)?;
            inner.extend_from_slice(&tlv(TAG_OCTET_STRING, value)?);
            (*fourcc, inner)
        }
    };
    let seq = tlv(TAG_SEQUENCE, &inner)?;
    inner.clear();

    // Private, constructed, high-tag-number element whose tag number is the
    // big-endian fourcc, wrapping the sequence.
    let mut out = vec![TAG_PRIVATE_HIGH];
    out.extend_from_slice(&base128(u32::from_be_bytes(fourcc)));
    out.extend_from_slice(&encode_len(seq.len())?);
    out.extend_from_slice(&seq);
    Ok(out)
}

/// Builds the `[1] EXPLICIT { SEQUENCE { IA5String "IM4R", SET { props } } }`
/// restore-info element.
fn build_im4r(props: &[RestoreProperty]) -> Result<Vec<u8>, IdeviceError> {
    let mut set_content = Vec::new();
    for p in props {
        set_content.extend_from_slice(&build_property(p)?);
    }
    let mut seq_content = tlv(TAG_IA5_STRING, b"IM4R")?;
    seq_content.extend_from_slice(&tlv(TAG_SET, &set_content)?);
    let seq = tlv(TAG_SEQUENCE, &seq_content)?;
    tlv(TAG_CONTEXT_1, &seq)
}

/// Encodes a DER BOOLEAN content octet (`0xFF` = true, `0x00` = false).
fn der_bool(value: bool) -> Vec<u8> {
    vec![if value { 0xFF } else { 0x00 }]
}

fn named_element(name: &[u8], value: &[u8]) -> Result<Vec<u8>, IdeviceError> {
    let mut tag4 = [0u8; 4];
    let n = name.len().min(4);
    tag4[..n].copy_from_slice(&name[..n]);
    let mut inner = tlv(TAG_IA5_STRING, name)?;
    inner.extend_from_slice(value);
    let seq = tlv(TAG_SEQUENCE, &inner)?;
    let mut out = vec![TAG_PRIVATE_HIGH];
    out.extend_from_slice(&base128(u32::from_be_bytes(tag4)));
    out.extend_from_slice(&encode_len(seq.len())?);
    out.extend_from_slice(&seq);
    Ok(out)
}

/// [`named_element`] for a plain four-character code.
fn fourcc_element(fourcc: [u8; 4], value: &[u8]) -> Result<Vec<u8>, IdeviceError> {
    named_element(&fourcc, value)
}

/// One component entry in a local `IM4M` manifest.
struct LocalManifestComponent {
    /// The element name: a fourcc for mapped components, or the full manifest key
    /// for unmapped ones (the full key is written as the IA5 string; only the
    /// ASN.1 tag number is truncated to four bytes).
    name: Vec<u8>,
    digest: Option<Vec<u8>>,
    /// `EKEY` (from the manifest's `Trusted`).
    ekey: Option<bool>,
    epro: Option<bool>,
    esec: Option<bool>,
    /// SEP Trust Boot Manifest digests (`TBMDigests`), written as `tbms` for
    /// `sepi` and `tbmr` for `rsep`. The SEP requires these to accept the manifest.
    tbm_digests: Option<Vec<u8>>,
}

/// Builds an unsigned ("local") `IM4M` manifest.
///
/// `IM4M ::= SEQUENCE { IA5String "IM4M", INTEGER 0, SET { MANB } }`, where `MANB`
/// wraps `MANP` (the board/chip properties) followed by one element per component.
fn build_local_manifest(
    board_id: u64,
    chip_id: u64,
    production_mode: bool,
    security_domain: u64,
    components: &[LocalManifestComponent],
) -> Result<Vec<u8>, IdeviceError> {
    // MANP: manifest properties.
    let mut props = Vec::new();
    props.extend(fourcc_element(
        *b"BORD",
        &tlv(TAG_INTEGER, &der_uint(board_id))?,
    )?);
    props.extend(fourcc_element(*b"CEPO", &tlv(TAG_INTEGER, &der_uint(0))?)?);
    props.extend(fourcc_element(
        *b"CHIP",
        &tlv(TAG_INTEGER, &der_uint(chip_id))?,
    )?);
    props.extend(fourcc_element(
        *b"CPRO",
        &tlv(TAG_BOOLEAN, &der_bool(production_mode))?,
    )?);
    props.extend(fourcc_element(
        *b"CSEC",
        &tlv(TAG_BOOLEAN, &der_bool(false))?,
    )?);
    props.extend(fourcc_element(
        *b"SDOM",
        &tlv(TAG_INTEGER, &der_uint(security_domain))?,
    )?);

    // MANB set: MANP followed by each component.
    let mut manb_content = fourcc_element(*b"MANP", &tlv(TAG_SET, &props)?)?;
    for c in components {
        let mut body = Vec::new();
        if let Some(digest) = &c.digest
            && !digest.is_empty()
        {
            body.extend(fourcc_element(*b"DGST", &tlv(TAG_OCTET_STRING, digest)?)?);
        }
        if let Some(v) = c.ekey {
            body.extend(fourcc_element(*b"EKEY", &tlv(TAG_BOOLEAN, &der_bool(v))?)?);
        }
        if let Some(v) = c.epro {
            body.extend(fourcc_element(*b"EPRO", &tlv(TAG_BOOLEAN, &der_bool(v))?)?);
        }
        if let Some(v) = c.esec {
            body.extend(fourcc_element(*b"ESEC", &tlv(TAG_BOOLEAN, &der_bool(v))?)?);
        }
        if let Some(tbm) = &c.tbm_digests {
            let tag = match c.name.as_slice() {
                b"sepi" => Some(*b"tbms"),
                b"rsep" => Some(*b"tbmr"),
                _ => None,
            };
            match tag {
                Some(tag) => {
                    body.extend(fourcc_element(tag, &tlv(TAG_OCTET_STRING, tbm)?)?);
                }
                None => tracing::warn!(
                    "unexpected TBMDigests for component {:?}; omitting",
                    std::str::from_utf8(&c.name)
                ),
            }
        }
        manb_content.extend(named_element(&c.name, &tlv(TAG_SET, &body)?)?);
    }
    let manb = fourcc_element(*b"MANB", &tlv(TAG_SET, &manb_content)?)?;

    let mut content = tlv(TAG_IA5_STRING, b"IM4M")?;
    content.extend(tlv(TAG_INTEGER, &der_uint(0))?);
    content.extend(tlv(TAG_SET, &manb)?);
    tlv(TAG_SEQUENCE, &content)
}

/// Builds the local `IM4M` manifest for a preboard stashbag request, from a
/// build identity.
///
/// `ApProductionMode`/`ApSecurityMode` are false and `ApSecurityDomain` is 1, and
/// only trusted firmware components are included, with their restore-request
/// rules applied.
pub fn build_preboard_manifest(
    build_identity: &plist::Dictionary,
    board_id: u64,
    chip_id: u64,
) -> Result<Vec<u8>, IdeviceError> {
    const SECURITY_DOMAIN: u64 = 1;

    let manifest = match build_identity.get("Manifest") {
        Some(plist::Value::Dictionary(m)) => m,
        _ => return Err(IdeviceError::BadBuildManifest),
    };

    // Parameters the restore-request rules are evaluated against.
    let parameters = crate::plist!({
        "ApProductionMode": false,
        "ApSecurityMode": false,
        "ApSupportsImg4": true,
    });
    let parameters = parameters.into_dictionary().unwrap();

    const SKIP_KEYS: &[&str] = &[
        "BasebandFirmware",
        "SE,UpdatePayload",
        "BaseSystem",
        "Diags",
        "Ap,ExclaveOS",
    ];
    const FW_FLAGS: &[&str] = &[
        "IsFirmwarePayload",
        "IsSecondaryFirmwarePayload",
        "IsFUDFirmware",
        "IsLoadedByiBoot",
        "IsEarlyAccessFirmware",
        "IsiBootEANFirmware",
        "IsiBootNonEssentialFirmware",
    ];

    let mut components = Vec::new();
    for (key, entry) in manifest {
        if SKIP_KEYS.contains(&key.as_str()) {
            continue;
        }
        let entry = match entry {
            plist::Value::Dictionary(d) => d,
            _ => continue,
        };
        let info = match entry.get("Info").and_then(plist::Value::as_dictionary) {
            Some(i) => i,
            None => continue,
        };
        let trusted = entry
            .get("Trusted")
            .and_then(plist::Value::as_boolean)
            .unwrap_or(false);

        // Only trusted firmware payloads.
        if !trusted {
            continue;
        }
        let is_fw = FW_FLAGS.iter().any(|f| {
            info.get(f)
                .and_then(plist::Value::as_boolean)
                .unwrap_or(false)
        });
        if !is_fw {
            continue;
        }
        if info
            .get("IsFTAB")
            .and_then(plist::Value::as_boolean)
            .unwrap_or(false)
        {
            continue;
        }

        // Prefer the manifest's own `Img4PayloadType`, then the known component
        // table. For anything still unmapped, use the full component key as the
        // element name
        let name: Vec<u8> = info
            .get("Img4PayloadType")
            .and_then(plist::Value::as_string)
            .map(|s| s.as_bytes().to_vec())
            .or_else(|| fourcc_for(key).map(|f| f.to_vec()))
            .unwrap_or_else(|| {
                tracing::debug!("preboard manifest: unmapped component `{key}`, using full key");
                key.as_bytes().to_vec()
            });

        // Apply restore-request rules against the (production-off) parameters.
        let mut node = entry.clone();
        node.remove("Info");
        if let Some(plist::Value::Array(rules)) = info.get("RestoreRequestRules") {
            crate::tss::apply_restore_request_rules(&mut node, &parameters, rules);
        }

        let digest = match node.get("Digest") {
            Some(plist::Value::Data(d)) => Some(d.clone()),
            // Trusted components without a digest carry an empty one.
            _ => Some(Vec::new()),
        };
        components.push(LocalManifestComponent {
            name,
            digest,
            ekey: Some(trusted),
            epro: node.get("EPRO").and_then(plist::Value::as_boolean),
            esec: node.get("ESEC").and_then(plist::Value::as_boolean),
            tbm_digests: match node.get("TBMDigests") {
                Some(plist::Value::Data(d)) => Some(d.clone()),
                _ => None,
            },
        });
    }

    build_local_manifest(board_id, chip_id, false, SECURITY_DOMAIN, &components)
}

/// Reads a single-byte-tag TLV at `off`, returning `(tag, content_len, content_start)`.
fn read_tlv(buf: &[u8], off: usize) -> Result<(u8, usize, usize), IdeviceError> {
    let err = || {
        IdeviceError::Restore(RestoreError::Img4(
            "truncated IM4P while locating fourcc".into(),
        ))
    };
    let tag = *buf.get(off).ok_or_else(err)?;
    let mut i = off + 1;
    let first = *buf.get(i).ok_or_else(err)?;
    i += 1;
    let len = if first < 0x80 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 8 {
            return Err(IdeviceError::Restore(RestoreError::Img4(
                "invalid IM4P length field".into(),
            )));
        }
        let mut l = 0usize;
        for _ in 0..n {
            l = (l << 8) | *buf.get(i).ok_or_else(err)? as usize;
            i += 1;
        }
        l
    };
    if i + len > buf.len() {
        return Err(err());
    }
    Ok((tag, len, i))
}

fn patch_im4p_fourcc(im4p: &[u8], fourcc: [u8; 4]) -> Result<Vec<u8>, IdeviceError> {
    let mut out = im4p.to_vec();

    let (t, _len, seq_start) = read_tlv(&out, 0)?;
    if t != TAG_SEQUENCE {
        return Err(IdeviceError::Restore(RestoreError::Img4(
            "IM4P is not a SEQUENCE".into(),
        )));
    }
    let (t0, l0, s0) = read_tlv(&out, seq_start)?;
    if t0 != TAG_IA5_STRING {
        return Err(IdeviceError::Restore(RestoreError::Img4(
            "IM4P magic is not an IA5String".into(),
        )));
    }
    let (t1, l1, s1) = read_tlv(&out, s0 + l0)?;
    if t1 != TAG_IA5_STRING || l1 != 4 {
        return Err(IdeviceError::Restore(RestoreError::Img4(
            "IM4P type is not a 4-byte IA5String".into(),
        )));
    }
    out[s1..s1 + 4].copy_from_slice(&fourcc);
    Ok(out)
}

/// Stitches an IPSW component into a personalized `IMG4`.
///
/// # Errors
/// Returns [`IdeviceError::Img4`] if the `IM4P` is malformed or a field cannot be
/// encoded.
pub fn stitch_component(
    im4p: &[u8],
    ap_img4_ticket: &[u8],
    fourcc_override: Option<[u8; 4]>,
    restore_properties: &[RestoreProperty],
) -> Result<Vec<u8>, IdeviceError> {
    let im4p = match fourcc_override {
        Some(f) => patch_im4p_fourcc(im4p, f)?,
        None => im4p.to_vec(),
    };

    let mut content = tlv(TAG_IA5_STRING, b"IMG4")?;
    content.extend_from_slice(&im4p);
    content.extend_from_slice(&tlv(TAG_CONTEXT_0, ap_img4_ticket)?);
    if !restore_properties.is_empty() {
        content.extend_from_slice(&build_im4r(restore_properties)?);
    }

    tlv(TAG_SEQUENCE, &content)
}

/// Returns the IMG4 four-character code for a build-manifest component name, if
/// known.
pub fn fourcc_for(component_name: &str) -> Option<[u8; 4]> {
    let s: &[u8; 4] = match component_name {
        "ACIBT" => b"acib",
        "ACIBTLPEM" => b"lpbt",
        "ACIWIFI" => b"aciw",
        "ANE" => b"anef",
        "ANS" => b"ansf",
        "AOP" => b"aopf",
        "AVE" => b"avef",
        "Alamo" => b"almo",
        "Ap,ANE1" => b"ane1",
        "Ap,ANE2" => b"ane2",
        "Ap,ANE3" => b"ane3",
        "Ap,AudioAccessibilityBootChime" => b"auac",
        "Ap,AudioBootChime" => b"aubt",
        "Ap,AudioPowerAttachChime" => b"aupr",
        "Ap,BootabilityBrainTrustCache" => b"trbb",
        "Ap,CIO" => b"ciof",
        "Ap,HapticAssets" => b"hpas",
        "Ap,LocalBoot" => b"lobo",
        "Ap,LocalPolicy" => b"lpol",
        "Ap,NextStageIM4MHash" => b"nsih",
        "Ap,RecoveryOSPolicyNonceHash" => b"ronh",
        "Ap,RestoreANE1" => b"ran1",
        "Ap,RestoreANE2" => b"ran2",
        "Ap,RestoreANE3" => b"ran3",
        "Ap,RestoreCIO" => b"rcio",
        "Ap,RestoreDCP2" => b"rdc2",
        "Ap,RestoreTMU" => b"rtmu",
        "Ap,Scorpius" => b"scpf",
        "Ap,SystemVolumeCanonicalMetadata" => b"msys",
        "Ap,TMU" => b"tmuf",
        "Ap,VolumeUUID" => b"vuid",
        "Ap,rOSLogo1" => b"rlg1",
        "Ap,rOSLogo2" => b"rlg2",
        "AppleLogo" => b"logo",
        "AudioCodecFirmware" => b"acfw",
        "BatteryCharging" => b"glyC",
        "BatteryCharging0" => b"chg0",
        "BatteryCharging1" => b"chg1",
        "BatteryFull" => b"batF",
        "BatteryLow0" => b"bat0",
        "BatteryLow1" => b"bat1",
        "BatteryPlugin" => b"glyP",
        "CFELoader" => b"cfel",
        "CrownFirmware" => b"crwn",
        "DCP" => b"dcpf",
        "Dali" => b"dali",
        "DeviceTree" => b"dtre",
        "Diags" => b"diag",
        "EngineeringTrustCache" => b"dtrs",
        "ExtDCP" => b"edcp",
        "GFX" => b"gfxf",
        "Hamm" => b"hamf",
        "Homer" => b"homr",
        "ISP" => b"ispf",
        "InputDevice" => b"ipdf",
        "KernelCache" => b"krnl",
        "LLB" => b"illb",
        "LeapHaptics" => b"lphp",
        "Liquid" => b"liqd",
        "LoadableTrustCache" => b"ltrs",
        "LowPowerWallet0" => b"lpw0",
        "LowPowerWallet1" => b"lpw1",
        "LowPowerWallet2" => b"lpw2",
        "MacEFI" => b"mefi",
        "MtpFirmware" => b"mtpf",
        "Multitouch" => b"mtfw",
        "NeedService" => b"nsrv",
        "OS" => return Some([b'O', b'S', 0, 0]),
        "OSRamdisk" => b"osrd",
        "PEHammer" => b"hmmr",
        "PERTOS" => b"pert",
        "PHLEET" => b"phlt",
        "PMP" => b"pmpf",
        "PersonalizedDMG" => b"pdmg",
        "RBM" => b"rmbt",
        "RTP" => b"rtpf",
        "Rap,SoftwareBinaryDsp1" => b"sbd1",
        "Rap,RTKitOS" => b"rkos",
        "Rap,RestoreRTKitOS" => b"rrko",
        "RecoveryMode" => b"recm",
        "RestoreANS" => b"rans",
        "RestoreDCP" => b"rdcp",
        "RestoreDeviceTree" => b"rdtr",
        "RestoreExtDCP" => b"recp",
        "RestoreKernelCache" => b"rkrn",
        "RestoreLogo" => b"rlgo",
        "RestoreRTP" => b"rrtp",
        "RestoreRamDisk" => b"rdsk",
        "RestoreSEP" => b"rsep",
        "RestoreTrustCache" => b"rtsc",
        "SCE" => b"scef",
        "SCE1Firmware" => b"sc1f",
        "SEP" => b"sepi",
        "SIO" => b"siof",
        "StaticTrustCache" => b"trst",
        "SystemLocker" => b"lckr",
        "SystemVolume" => b"isys",
        "WCHFirmwareUpdater" => b"wchf",
        "ftap" => b"ftap",
        "ftsp" => b"ftsp",
        "iBEC" => b"ibec",
        "iBSS" => b"ibss",
        "iBoot" => b"ibot",
        "iBootData" => b"ibdt",
        "iBootDataStage1" => b"ibd1",
        "iBootTest" => b"itst",
        "rfta" => b"rfta",
        "rfts" => b"rfts",
        "Ap,DCP2" => b"dcp2",
        "Ap,RestoreSecureM3Firmware" => b"rsm3",
        "Ap,RestoreSecurePageTableMonitor" => b"rspt",
        "Ap,RestoreTrustedExecutionMonitor" => b"rtrx",
        "Ap,RestorecL4" => b"rxcl",
        _ => return None,
    };
    Some(*s)
}

/// Returns the fourcc a `Restore` component must be re-tagged with, if any
pub fn restore_fourcc_override(component_name: &str) -> Option<[u8; 4]> {
    const RETAGGED: &[&str] = &[
        "RestoreKernelCache",
        "RestoreDeviceTree",
        "RestoreSEP",
        "RestoreLogo",
        "RestoreTrustCache",
        "RestoreDCP",
        "Ap,RestoreDCP2",
        "Ap,RestoreTMU",
        "Ap,RestoreCIO",
        "Ap,DCP2",
        "Ap,RestoreSecureM3Firmware",
        "Ap,RestoreSecurePageTableMonitor",
        "Ap,RestoreTrustedExecutionMonitor",
        "Ap,RestorecL4",
    ];
    if RETAGGED.contains(&component_name) {
        fourcc_for(component_name)
    } else {
        None
    }
}
