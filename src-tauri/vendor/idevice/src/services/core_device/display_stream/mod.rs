// Jackson Coxson
//
// Device screen streaming over `com.apple.coredevice.displayservice`.
//
// Pipeline:
//   1. Connect `DisplayServiceClient` over RemoteXPC (RSD).
//   2. Build a `MediaNegotiationBlob` offer, zlib it (`negotiation`), and send
//      it via `startvideooutput` with the receiver address of a UDP socket we
//      open on the tunnel.
//   3. Parse the device's answer (same blob format) for the negotiated config.
//   4. Receive plaintext RTP/HEVC (`rtp`) and reassemble/decode.
//
// The media transport carries no SRTP (negotiated cipher suite is 0), so the
// whole path is reproducible in userspace.

mod client;
pub mod hevc;
pub mod negotiation;
mod protobuf;
pub mod rtcp;
pub mod rtp;

pub use client::{
    DisplayServiceClient, build_start_audio_parameters, build_start_video_parameters,
};
pub use hevc::HevcDepacketizer;
pub use negotiation::{
    CallInfoBlob, MediaNegotiationBlob, build_screen_audio_offer, build_screen_video_offer,
    parse_answer_media_blob,
};
pub use rtcp::{
    ReportBlock, SenderReport, build_fir, build_frame_ack, build_keyframe_request, build_liveness,
    build_pli, build_rctl, build_receiver_report, build_sdes, is_rtcp,
};
pub use rtp::RtpPacket;
