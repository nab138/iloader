// Jackson Coxson
//
// VCMediaNegotiationBlobV2 - the AVConference media-negotiation blob carried as
// the `negotiatorOffer` (and returned as the device's answer) by the CoreDevice
// displayservice.
//
//     negotiatorOffer = zlib( protobuf( VCMediaNegotiationBlobV2 ) )
//
// Schema + field numbers are in `media_negotiation.proto` (reverse-engineered
// from AVConference). We only model the messages needed for a device->host
// video (screen) stream

use std::io::{Read, Write};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};

use super::super::CoreDeviceError;
use super::protobuf::{Decoder, Encoder, Field};
use crate::IdeviceError;

/// Top-level negotiation blob. Optional sub-messages are `None` when absent.
#[derive(Debug, Default, Clone)]
pub struct MediaNegotiationBlob {
    pub general_info: Option<GeneralInfo>,
    pub bandwidth_settings: Option<BandwidthSettings>,
    pub codec_features: Option<CodecFeatures>,
    pub stream_groups: Vec<StreamGroup>,
}

#[derive(Debug, Default, Clone)]
pub struct GeneralInfo {
    pub ntp_time: u64,
    pub cname: String,
    pub ab_switches: u32,
    pub screen_res: u32,
    pub fec_header_version: u32,
    pub rtx_version: u32,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BandwidthSettings {
    pub cap_2g: u32,
    pub cap_3g: u32,
    pub cap_lte: u32,
    pub cap_5g: u32,
    pub cap_wifi: u32,
}

#[derive(Debug, Default, Clone)]
pub struct CodecFeatures {
    pub audio_features: u32,
    pub video_features: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
pub struct StreamGroup {
    pub stream_group: u32,
    pub payloads: Vec<StreamGroupPayload>,
    pub streams: Vec<StreamGroupStream>,
    pub settings_u1: Option<SettingsU1>,
}

#[derive(Debug, Default, Clone)]
pub struct StreamGroupPayload {
    pub codec_type: u32,
    pub rtp_payload: u32,
    pub p_time: u32,
    pub rtcp_flags: u32,
    pub media_flags: u32,
    pub profile_level_id: u32,
    pub rtp_sample_rate: u32,
    pub cipher_suite: u32,
    pub packed_payload: Vec<u8>,
    pub encoder_usage: u32,
}

#[derive(Debug, Default, Clone)]
pub struct StreamGroupStream {
    pub metadata: u32,
    pub payload_spec_or_payloads: u32,
    pub quality_index: u32,
    pub rtp_ssrc: u32,
    pub stream_id: u32,
    pub max_network_bitrate: u32,
    pub repaired_max_network_bitrate: u32,
    pub audio_channel_count: u32,
    pub stream_index: u32,
    pub required_packed_payload: Vec<u8>,
    pub optional_packed_payload: Vec<u8>,
    pub coordinate_system: u32,
    pub payloads_version: u32,
    pub max_network_bitrate_v2: u32,
    pub repaired_max_network_bitrate_v2: u32,
}

#[derive(Debug, Default, Clone)]
pub struct SettingsU1 {
    pub rtp_ssrc: u32,
    pub encode_decode_features: Vec<EncodeDecodeFeatures>,
}

#[derive(Debug, Default, Clone)]
pub struct EncodeDecodeFeatures {
    pub rtp_payload: u32,
    pub encode_decode_features: Vec<u8>,
}

pub const STREAM_GROUP_SCREEN: u32 = 3;
pub const SCREEN_FLS: &[u8] = b"FLS;VRAE:0;SW:1;";

/// So we're using a different on than Device Hub, but this seems to stop screen tearing,
/// so we'll use this one instead.
pub const SCREEN_HEVC_FLS: &str =
    "FLS;MS:-1;LF:-1;LTR;CABAC;POS:0;EOD:1;HTS:2;RR:3;AR:16/9,5/8;XR:16/9,5/8;";

pub const NEGOTIATOR_MODE_COREDEVICE_SCREEN: i64 = 5;
pub const NEGOTIATOR_MODE_COREDEVICE_AUDIO: i64 = 6;

const KEY_CALL_ID: &str = "avcMediaStreamOptionCallID";
const KEY_MEDIA_BLOB: &str = "avcMediaStreamNegotiatorMediaBlob";
const KEY_MODE: &str = "avcMediaStreamNegotiatorMode";
const KEY_REMOTE_ENDPOINT_INFO: &str = "avcMediaStreamOptionRemoteEndpointInfo";

/// `VCCallInfoBlob` - endpoint metadata embedded (as protobuf) under
/// `avcMediaStreamOptionRemoteEndpointInfo` in the offer. Schema reversed from
/// `-[VCCallInfoBlob writeTo:]`.
#[derive(Debug, Clone, Default)]
pub struct CallInfoBlob {
    pub call_id: u32,
    pub client_version: u32,
    pub device_type: String,
    pub framework_version: String,
    pub os_version: String,
    pub device_name: Option<String>,
    pub audio_device_uid: Option<String>,
}

impl CallInfoBlob {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.uint_field(1, self.call_id as u64);
        e.uint_field(2, self.client_version as u64);
        e.string_field(3, &self.device_type);
        e.string_field(4, &self.framework_version);
        e.string_field(5, &self.os_version);
        if let Some(n) = &self.device_name {
            e.string_field(6, n);
        }
        if let Some(u) = &self.audio_device_uid {
            e.string_field(7, u);
        }
        e.into_bytes()
    }
}

/// Build the CoreDevice screen-sharing media blob
pub fn build_screen_media_blob(session_id: u32, ntp_time: u64) -> Vec<u8> {
    build_screen_media_blob_with_fls(session_id, ntp_time, SCREEN_HEVC_FLS)
}

fn build_screen_media_blob_with_fls(session_id: u32, ntp_time: u64, hevc_fls: &str) -> Vec<u8> {
    fn format_desc(e: &mut Encoder, variant: u64) {
        e.message_field(2, |m| {
            m.uint_field(1, 1);
            m.uint_field(2, variant);
            m.uint_field(3, 0xc3c3);
            m.uint_field(4, 0);
        });
    }

    let mut e = Encoder::new();
    e.uint_field(1, 1);
    e.uint_field(2, 1);
    e.message_field(5, |m| {
        m.uint_field(1, session_id as u64);
        m.uint_field(2, 0);
        // H.264 (RTP PT 123), four format descriptors.
        m.message_field(3, |c| {
            c.uint_field(1, 123);
            format_desc(c, 1);
            format_desc(c, 2);
            format_desc(c, 1);
            format_desc(c, 2);
            c.string_field(3, "FLS;SW:1;");
            c.uint_field(4, 1);
        });
        // HEVC (RTP PT 100), two format descriptors.
        m.message_field(3, |c| {
            c.uint_field(1, 100);
            format_desc(c, 1);
            format_desc(c, 2);
            c.string_field(3, hevc_fls);
            c.uint_field(4, 14);
        });
        m.uint_field(7, 1);
        m.uint_field(8, 63);
        m.uint_field(12, 1);
    });
    write_blob_tail(&mut e, ntp_time);
    e.into_bytes()
}

/// Build the CoreDevice screen-sharing audio media blob
pub fn build_audio_media_blob(session_id: u32, ntp_time: u64) -> Vec<u8> {
    let mut e = Encoder::new();
    e.uint_field(1, 1);
    e.uint_field(2, 1);
    e.message_field(3, |m| {
        m.uint_field(1, session_id as u64);
        m.uint_field(2, 0);
        m.uint_field(3, 0);
        m.uint_field(4, 24_191);
        m.uint_field(5, 0);
        m.uint_field(6, 0);
    });
    write_blob_tail(&mut e, ntp_time);
    e.into_bytes()
}

/// Write the fields shared by the audio and video screen blobs: the UserAgent
/// (field 6), field 8, the bandwidth/parameter ladder (field 9, repeated), and
/// the trailing scalars (13 = NTP, 14, 16, 18).
fn write_blob_tail(e: &mut Encoder, ntp_time: u64) {
    fn param(e: &mut Encoder, id: u64, value: u64, flags: Option<u64>) {
        e.message_field(9, |m| {
            m.uint_field(1, id);
            m.uint_field(2, value);
            if let Some(f) = flags {
                m.uint_field(3, f);
            }
        });
    }
    e.string_field(6, "Viceroy 1.7.0");
    e.uint_field(8, 0);
    param(e, 0, 20_000_000, Some(0x18000));
    param(e, 4074, 0, Some(0x4000));
    param(e, 0, 60_000_000, Some(0x40000));
    param(e, 0, 40_000_000, Some(0x3000));
    param(e, 16, 4100, None);
    param(e, 0, 6_000_000, Some(0x20000));
    param(e, 4, 6500, None);
    param(e, 0, 100_000_000, Some(0x100000));
    param(e, 1, 299, None);
    param(e, 0, 75_000_000, Some(0x80000));
    e.uint_field(13, ntp_time);
    e.uint_field(14, 2);
    e.uint_field(16, 0);
    e.uint_field(18, 1);
}

/// zlib-compress a media blob into the form embedded in the offer plist.
fn zlib_compress(raw: &[u8]) -> Result<Vec<u8>, IdeviceError> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(raw)
        .map_err(|e| CoreDeviceError::Negotiation(format!("zlib compress: {e}")))?;
    z.finish()
        .map_err(|e| CoreDeviceError::Negotiation(format!("zlib finish: {e}")).into())
}

/// Build the full `negotiatorOffer` for a device->host screen video stream:
/// a binary plist matching what Device Hub sends.
///
/// Keys:
/// - `avcMediaStreamOptionCallID`        - UUID string identifying the session
/// - `avcMediaStreamNegotiatorMediaBlob` - zlib-compressed screen media blob
/// - `avcMediaStreamNegotiatorMode`      - 5 (CoreDeviceScreenSharing; the audio
///   offer uses 6 / CoreDeviceSystemAudio
/// - `avcMediaStreamOptionRemoteEndpointInfo` - `VCCallInfoBlob` protobuf
pub fn build_screen_video_offer(
    call_id: &str,
    call_info: &CallInfoBlob,
    ssrc: u32,
) -> Result<Vec<u8>, IdeviceError> {
    let blob = build_screen_media_blob(ssrc, ntp_now());
    build_offer_plist(call_id, call_info, &blob, NEGOTIATOR_MODE_COREDEVICE_SCREEN)
}

/// Build the audio screen-sharing `negotiatorOffer`. Sent before the video
/// offer to establish the screen-sharing session.
pub fn build_screen_audio_offer(
    call_id: &str,
    call_info: &CallInfoBlob,
) -> Result<Vec<u8>, IdeviceError> {
    let session_id = uuid::Uuid::new_v4().as_u128() as u32;
    let blob = build_audio_media_blob(session_id, ntp_now());
    build_offer_plist(call_id, call_info, &blob, NEGOTIATOR_MODE_COREDEVICE_AUDIO)
}

/// Wrap a media blob into the binary-plist `negotiatorOffer`.
fn build_offer_plist(
    call_id: &str,
    call_info: &CallInfoBlob,
    media_blob_raw: &[u8],
    negotiator_mode: i64,
) -> Result<Vec<u8>, IdeviceError> {
    let media_blob = zlib_compress(media_blob_raw)?;

    let mut dict = plist::Dictionary::new();
    dict.insert(KEY_CALL_ID.into(), plist::Value::String(call_id.into()));
    dict.insert(KEY_MEDIA_BLOB.into(), plist::Value::Data(media_blob));
    dict.insert(
        KEY_MODE.into(),
        plist::Value::Integer(negotiator_mode.into()),
    );
    dict.insert(
        KEY_REMOTE_ENDPOINT_INFO.into(),
        plist::Value::Data(call_info.encode()),
    );

    let mut out = Vec::new();
    plist::to_writer_binary(&mut out, &plist::Value::Dictionary(dict))
        .map_err(|e| CoreDeviceError::Negotiation(format!("offer plist encode: {e}")))?;
    Ok(out)
}

/// Parse the device's answer (`StartResponse.negotiatorAnswer`): a binary plist
/// with the device's `VCMediaNegotiationBlobV2` under `avcMediaStreamNegotiatorMediaBlob`.
pub fn parse_answer_media_blob(answer: &[u8]) -> Result<MediaNegotiationBlob, IdeviceError> {
    let value: plist::Value = plist::from_bytes(answer)
        .map_err(|e| CoreDeviceError::Negotiation(format!("answer plist decode: {e}")))?;
    let blob = value
        .as_dictionary()
        .and_then(|d| d.get(KEY_MEDIA_BLOB))
        .and_then(|v| v.as_data())
        .ok_or(CoreDeviceError::MissingField(KEY_MEDIA_BLOB))?;
    Ok(MediaNegotiationBlob::decode(blob))
}

fn ntp_now() -> u64 {
    const UNIX_TO_NTP: u64 = 2_208_988_800; // seconds between 1900 and 1970
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() + UNIX_TO_NTP;
    let frac = ((now.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

impl MediaNegotiationBlob {
    /// Build a best-effort offer for a device->host HEVC screen video stream.
    pub fn video_offer(ssrc: u32) -> Self {
        MediaNegotiationBlob {
            general_info: Some(GeneralInfo {
                ntp_time: ntp_now(),
                ..Default::default()
            }),
            bandwidth_settings: None,
            codec_features: Some(CodecFeatures {
                audio_features: 0,
                video_features: SCREEN_FLS.to_vec(),
            }),
            stream_groups: vec![StreamGroup {
                stream_group: STREAM_GROUP_SCREEN,
                payloads: vec![StreamGroupPayload {
                    codec_type: 1,
                    rtp_payload: 100,
                    cipher_suite: 0,
                    ..Default::default()
                }],
                streams: vec![StreamGroupStream {
                    rtp_ssrc: ssrc,
                    max_network_bitrate: 6_000_000,
                    ..Default::default()
                }],
                settings_u1: Some(SettingsU1 {
                    rtp_ssrc: ssrc,
                    encode_decode_features: vec![EncodeDecodeFeatures {
                        rtp_payload: 100,
                        encode_decode_features: SCREEN_FLS.to_vec(),
                    }],
                }),
            }],
        }
    }
}

impl MediaNegotiationBlob {
    /// Serialize to protobuf bytes
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        if let Some(g) = &self.general_info {
            e.message_field(1, |m| g.encode_into(m));
        }
        if let Some(b) = &self.bandwidth_settings {
            e.message_field(2, |m| b.encode_into(m));
        }
        if let Some(c) = &self.codec_features {
            e.message_field(3, |m| c.encode_into(m));
        }
        for sg in &self.stream_groups {
            e.message_field(7, |m| sg.encode_into(m));
        }
        e.into_bytes()
    }

    /// Serialize and zlib-compress into the wire `negotiatorOffer` form.
    pub fn to_negotiator_offer(&self) -> Result<Vec<u8>, IdeviceError> {
        let raw = self.encode();
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&raw)
            .map_err(|e| CoreDeviceError::Negotiation(format!("zlib compress: {e}")))?;
        z.finish()
            .map_err(|e| CoreDeviceError::Negotiation(format!("zlib finish: {e}")).into())
    }

    /// Decompress + parse a wire `negotiatorOffer`/answer blob.
    pub fn from_negotiator_offer(data: &[u8]) -> Result<Self, IdeviceError> {
        let mut z = ZlibDecoder::new(data);
        let mut raw = Vec::new();
        z.read_to_end(&mut raw)
            .map_err(|e| CoreDeviceError::Negotiation(format!("zlib decompress: {e}")))?;
        Ok(Self::decode(&raw))
    }

    /// Parse uncompressed protobuf bytes.
    pub fn decode(buf: &[u8]) -> Self {
        let mut out = MediaNegotiationBlob::default();
        let mut d = Decoder::new(buf);
        while let Some((field, val)) = d.next_field() {
            match (field, val) {
                (1, Field::Len(b)) => out.general_info = Some(GeneralInfo::decode(b)),
                (2, Field::Len(b)) => out.bandwidth_settings = Some(BandwidthSettings::decode(b)),
                (3, Field::Len(b)) => out.codec_features = Some(CodecFeatures::decode(b)),
                (7, Field::Len(b)) => out.stream_groups.push(StreamGroup::decode(b)),
                _ => {}
            }
        }
        out
    }
}

impl GeneralInfo {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.ntp_time);
        if !self.cname.is_empty() {
            e.string_field(2, &self.cname);
        }
        e.uint_field(3, self.ab_switches as u64);
        e.uint_field(4, self.screen_res as u64);
        e.uint_field(5, self.fec_header_version as u64);
        e.uint_field(6, self.rtx_version as u64);
    }

    fn decode(buf: &[u8]) -> Self {
        let mut g = GeneralInfo::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => g.ntp_time = x,
                (2, Field::Len(b)) => g.cname = String::from_utf8_lossy(b).into_owned(),
                (3, Field::Varint(x)) => g.ab_switches = x as u32,
                (4, Field::Varint(x)) => g.screen_res = x as u32,
                (5, Field::Varint(x)) => g.fec_header_version = x as u32,
                (6, Field::Varint(x)) => g.rtx_version = x as u32,
                _ => {}
            }
        }
        g
    }
}

impl BandwidthSettings {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.cap_2g as u64);
        e.uint_field(2, self.cap_3g as u64);
        e.uint_field(3, self.cap_lte as u64);
        e.uint_field(4, self.cap_5g as u64);
        e.uint_field(5, self.cap_wifi as u64);
    }

    fn decode(buf: &[u8]) -> Self {
        let mut s = BandwidthSettings::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            if let Field::Varint(x) = v {
                match f {
                    1 => s.cap_2g = x as u32,
                    2 => s.cap_3g = x as u32,
                    3 => s.cap_lte = x as u32,
                    4 => s.cap_5g = x as u32,
                    5 => s.cap_wifi = x as u32,
                    _ => {}
                }
            }
        }
        s
    }
}

impl CodecFeatures {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.audio_features as u64);
        if !self.video_features.is_empty() {
            e.bytes_field(2, &self.video_features);
        }
    }

    fn decode(buf: &[u8]) -> Self {
        let mut c = CodecFeatures::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => c.audio_features = x as u32,
                (2, Field::Len(b)) => c.video_features = b.to_vec(),
                _ => {}
            }
        }
        c
    }
}

impl StreamGroup {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.stream_group as u64);
        for p in &self.payloads {
            e.message_field(2, |m| p.encode_into(m));
        }
        for s in &self.streams {
            e.message_field(3, |m| s.encode_into(m));
        }
        if let Some(s) = &self.settings_u1 {
            e.message_field(4, |m| s.encode_into(m));
        }
    }

    fn decode(buf: &[u8]) -> Self {
        let mut g = StreamGroup::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => g.stream_group = x as u32,
                (2, Field::Len(b)) => g.payloads.push(StreamGroupPayload::decode(b)),
                (3, Field::Len(b)) => g.streams.push(StreamGroupStream::decode(b)),
                (4, Field::Len(b)) => g.settings_u1 = Some(SettingsU1::decode(b)),
                _ => {}
            }
        }
        g
    }
}

impl StreamGroupPayload {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.codec_type as u64);
        e.uint_field(2, self.rtp_payload as u64);
        e.uint_field(3, self.p_time as u64);
        e.uint_field(4, self.rtcp_flags as u64);
        e.uint_field(5, self.media_flags as u64);
        e.uint_field(6, self.profile_level_id as u64);
        e.uint_field(7, self.rtp_sample_rate as u64);
        e.uint_field(8, self.cipher_suite as u64);
        if !self.packed_payload.is_empty() {
            e.bytes_field(9, &self.packed_payload);
        }
        e.uint_field(10, self.encoder_usage as u64);
    }

    fn decode(buf: &[u8]) -> Self {
        let mut p = StreamGroupPayload::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => p.codec_type = x as u32,
                (2, Field::Varint(x)) => p.rtp_payload = x as u32,
                (3, Field::Varint(x)) => p.p_time = x as u32,
                (4, Field::Varint(x)) => p.rtcp_flags = x as u32,
                (5, Field::Varint(x)) => p.media_flags = x as u32,
                (6, Field::Varint(x)) => p.profile_level_id = x as u32,
                (7, Field::Varint(x)) => p.rtp_sample_rate = x as u32,
                (8, Field::Varint(x)) => p.cipher_suite = x as u32,
                (9, Field::Len(b)) => p.packed_payload = b.to_vec(),
                (10, Field::Varint(x)) => p.encoder_usage = x as u32,
                _ => {}
            }
        }
        p
    }
}

impl StreamGroupStream {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.metadata as u64);
        e.uint_field(2, self.payload_spec_or_payloads as u64);
        e.uint_field(3, self.quality_index as u64);
        e.uint_field(4, self.rtp_ssrc as u64);
        e.uint_field(5, self.stream_id as u64);
        e.uint_field(6, self.max_network_bitrate as u64);
        e.uint_field(7, self.repaired_max_network_bitrate as u64);
        e.uint_field(8, self.audio_channel_count as u64);
        e.uint_field(9, self.stream_index as u64);
        if !self.required_packed_payload.is_empty() {
            e.bytes_field(10, &self.required_packed_payload);
        }
        if !self.optional_packed_payload.is_empty() {
            e.bytes_field(11, &self.optional_packed_payload);
        }
        e.uint_field(12, self.coordinate_system as u64);
        e.uint_field(13, self.payloads_version as u64);
        e.uint_field(14, self.max_network_bitrate_v2 as u64);
        e.uint_field(15, self.repaired_max_network_bitrate_v2 as u64);
    }

    fn decode(buf: &[u8]) -> Self {
        let mut s = StreamGroupStream::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => s.metadata = x as u32,
                (2, Field::Varint(x)) => s.payload_spec_or_payloads = x as u32,
                (3, Field::Varint(x)) => s.quality_index = x as u32,
                (4, Field::Varint(x)) => s.rtp_ssrc = x as u32,
                (5, Field::Varint(x)) => s.stream_id = x as u32,
                (6, Field::Varint(x)) => s.max_network_bitrate = x as u32,
                (7, Field::Varint(x)) => s.repaired_max_network_bitrate = x as u32,
                (8, Field::Varint(x)) => s.audio_channel_count = x as u32,
                (9, Field::Varint(x)) => s.stream_index = x as u32,
                (10, Field::Len(b)) => s.required_packed_payload = b.to_vec(),
                (11, Field::Len(b)) => s.optional_packed_payload = b.to_vec(),
                (12, Field::Varint(x)) => s.coordinate_system = x as u32,
                (13, Field::Varint(x)) => s.payloads_version = x as u32,
                (14, Field::Varint(x)) => s.max_network_bitrate_v2 = x as u32,
                (15, Field::Varint(x)) => s.repaired_max_network_bitrate_v2 = x as u32,
                _ => {}
            }
        }
        s
    }
}

impl SettingsU1 {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.rtp_ssrc as u64);
        for edf in &self.encode_decode_features {
            e.message_field(2, |m| edf.encode_into(m));
        }
    }

    fn decode(buf: &[u8]) -> Self {
        let mut s = SettingsU1::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => s.rtp_ssrc = x as u32,
                (2, Field::Len(b)) => s
                    .encode_decode_features
                    .push(EncodeDecodeFeatures::decode(b)),
                _ => {}
            }
        }
        s
    }
}

impl EncodeDecodeFeatures {
    fn encode_into(&self, e: &mut Encoder) {
        e.uint_field(1, self.rtp_payload as u64);
        if !self.encode_decode_features.is_empty() {
            e.bytes_field(2, &self.encode_decode_features);
        }
    }

    fn decode(buf: &[u8]) -> Self {
        let mut s = EncodeDecodeFeatures::default();
        let mut d = Decoder::new(buf);
        while let Some((f, v)) = d.next_field() {
            match (f, v) {
                (1, Field::Varint(x)) => s.rtp_payload = x as u32,
                (2, Field::Len(b)) => s.encode_decode_features = b.to_vec(),
                _ => {}
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrips_through_zlib() {
        let blob = MediaNegotiationBlob {
            general_info: Some(GeneralInfo {
                cname: "host".into(),
                rtx_version: 1,
                ..Default::default()
            }),
            stream_groups: vec![StreamGroup {
                stream_group: 1,
                payloads: vec![StreamGroupPayload {
                    rtp_payload: 100,
                    rtp_sample_rate: 90000,
                    ..Default::default()
                }],
                streams: vec![StreamGroupStream {
                    rtp_ssrc: 0xdead_beef,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let wire = blob.to_negotiator_offer().unwrap();
        let back = MediaNegotiationBlob::from_negotiator_offer(&wire).unwrap();
        assert_eq!(back.general_info.as_ref().unwrap().cname, "host");
        assert_eq!(back.stream_groups.len(), 1);
        assert_eq!(back.stream_groups[0].payloads[0].rtp_payload, 100);
        assert_eq!(back.stream_groups[0].streams[0].rtp_ssrc, 0xdead_beef);
    }
}
