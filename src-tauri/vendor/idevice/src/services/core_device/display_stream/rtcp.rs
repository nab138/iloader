// Jackson Coxson
//
// RTCP for the CoreDevice display stream (RFC 3550, plus AVPF feedback from
// RFC 4585 / RFC 5104). The media transport is plaintext (the negotiated
// SRTPCipherSuite is 0), so RTCP is plaintext too.

/// RTCP packet types (RFC 3550 §12.1, RFC 4585 §6).
pub const PT_SENDER_REPORT: u8 = 200;
pub const PT_RECEIVER_REPORT: u8 = 201;
pub const PT_SDES: u8 = 202;
pub const PT_BYE: u8 = 203;
pub const PT_APP: u8 = 204;
pub const PT_RTPFB: u8 = 205;
pub const PT_PSFB: u8 = 206;

/// PSFB feedback message types (the low 5 bits of the first byte).
const FMT_PLI: u8 = 1; // Picture Loss Indication
const FMT_FIR: u8 = 4; // Full Intra Request

pub fn is_rtcp(buf: &[u8]) -> bool {
    buf.len() >= 2 && (PT_SENDER_REPORT..=PT_PSFB).contains(&buf[1])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderReport {
    pub ssrc: u32,
    pub ntp_middle: u32,
}

impl SenderReport {
    /// Find and parse the first Sender Report in a (possibly compound) RTCP
    /// datagram. Returns `None` if there is no SR.
    pub fn parse_first(buf: &[u8]) -> Option<SenderReport> {
        let mut off = 0;
        // Walk the compound packet, each sub-packet's length is in 32-bit words
        // minus one.
        while off + 4 <= buf.len() {
            let pt = buf[off + 1];
            let len_words = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
            let pkt_len = (len_words + 1) * 4;
            if pkt_len == 0 || off + pkt_len > buf.len() {
                break;
            }
            // SR body: sender SSRC (4) | NTP MSW (4) | NTP LSW (4) | ...
            if pt == PT_SENDER_REPORT && pkt_len >= 16 {
                let ssrc =
                    u32::from_be_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);
                let ntp_msw =
                    u32::from_be_bytes([buf[off + 8], buf[off + 9], buf[off + 10], buf[off + 11]]);
                let ntp_lsw = u32::from_be_bytes([
                    buf[off + 12],
                    buf[off + 13],
                    buf[off + 14],
                    buf[off + 15],
                ]);
                let ntp_middle = (ntp_msw << 16) | (ntp_lsw >> 16);
                return Some(SenderReport { ssrc, ntp_middle });
            }
            off += pkt_len;
        }
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReportBlock {
    pub source_ssrc: u32,
    pub fraction_lost: u8,
    pub cumulative_lost: u32,
    pub highest_seq: u32,
    pub jitter: u32,
    pub lsr: u32,
    pub dlsr: u32,
}

fn patch_length(out: &mut [u8], start: usize) {
    let words = ((out.len() - start) / 4).saturating_sub(1) as u16;
    out[start + 2..start + 4].copy_from_slice(&words.to_be_bytes());
}

pub fn build_receiver_report(our_ssrc: u32, blocks: &[ReportBlock]) -> Vec<u8> {
    let rc = blocks.len().min(31);
    let mut out = Vec::with_capacity(8 + rc * 24);
    out.push(0x80 | rc as u8); // V=2, P=0, RC
    out.push(PT_RECEIVER_REPORT);
    out.extend_from_slice(&[0, 0]); // length, patched below
    out.extend_from_slice(&our_ssrc.to_be_bytes());
    for b in blocks.iter().take(31) {
        out.extend_from_slice(&b.source_ssrc.to_be_bytes());
        out.push(b.fraction_lost);
        // Low 24 bits of the cumulative loss count.
        out.extend_from_slice(&b.cumulative_lost.to_be_bytes()[1..]);
        out.extend_from_slice(&b.highest_seq.to_be_bytes());
        out.extend_from_slice(&b.jitter.to_be_bytes());
        out.extend_from_slice(&b.lsr.to_be_bytes());
        out.extend_from_slice(&b.dlsr.to_be_bytes());
    }
    patch_length(&mut out, 0);
    out
}

pub fn build_sdes(our_ssrc: u32, cname: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x80 | 1); // V=2, SC=1 (one chunk)
    out.push(PT_SDES);
    out.extend_from_slice(&[0, 0]); // length, patched below
    out.extend_from_slice(&our_ssrc.to_be_bytes());
    // CNAME item: type 1, length, text.
    let bytes = cname.as_bytes();
    let len = bytes.len().min(255);
    out.push(1);
    out.push(len as u8);
    out.extend_from_slice(&bytes[..len]);
    // Items end with a type-0 octet; then pad to a 32-bit boundary.
    out.push(0);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    patch_length(&mut out, 0);
    out
}

/// Build a Picture Loss Indication asking `media_ssrc` for a fresh
/// keyframe
pub fn build_pli(our_ssrc: u32, media_ssrc: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.push(0x80 | FMT_PLI); // V=2, FMT=PLI
    out.push(PT_PSFB);
    out.extend_from_slice(&[0, 0]); // length, patched below
    out.extend_from_slice(&our_ssrc.to_be_bytes());
    out.extend_from_slice(&media_ssrc.to_be_bytes());
    patch_length(&mut out, 0);
    out
}

/// Build a Full Intra Request for `media_ssrc`
pub fn build_fir(our_ssrc: u32, media_ssrc: u32, seq_nr: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(20);
    out.push(0x80 | FMT_FIR); // V=2, FMT=FIR
    out.push(PT_PSFB);
    out.extend_from_slice(&[0, 0]); // length, patched below
    out.extend_from_slice(&our_ssrc.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&media_ssrc.to_be_bytes());
    out.push(seq_nr);
    out.extend_from_slice(&[0, 0, 0]);
    patch_length(&mut out, 0);
    out
}

/// The 4-byte "name" of the per-frame acknowledgment APP packet (PT 204), as
/// captured from Apple's Device Hub.
const APP_NAME_FRAME_ACK: [u8; 4] = [0x00, 0x00, 0x00, 0x05];

pub fn build_frame_ack(our_ssrc: u32, rtp_timestamp: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.push(0x80); // V=2, P=0, subtype=0
    out.push(PT_APP);
    out.extend_from_slice(&[0, 0]); // length, patched below
    out.extend_from_slice(&our_ssrc.to_be_bytes());
    out.extend_from_slice(&APP_NAME_FRAME_ACK);
    out.extend_from_slice(&rtp_timestamp.to_be_bytes());
    patch_length(&mut out, 0);
    out
}

/// Build an `RCTL` receiver-control report (RTCP APP, PT 204), the periodic
/// feedback Apple's Device Hub sends roughly every 50ms (alongside the per-frame ACK).
/// AVConference's encoder relies on this for reference/rate management; without
/// it the encoder drifts out of sync with the receiver during heavy motion and
/// the picture corrupts with no decode errors.
///
/// 32-byte wire format, fields reverse-engineered from a capture:
///   `80 cc 00 07` | our SSRC (4) | `RCTL` (4) | 20-byte body:
///     [0..4]  = `85 00 00 04` (constant tag)
///     [4..6]  = frame counter (received frames, big-endian)
///     [6..8]  = 0
///     [8..12] = 0  (per-interval metric; 0 = nominal)
///     [12..14]= millisecond clock (for RTT; only the delta matters)
///     [14..16]= 0
///     [16..18]= highest RTP sequence number received, relative to the base seq
///     [18..20]= 0
/// The loss/jitter-ish fields are sent as 0, which is accurate for a lossless
/// link (matching what Apple sent on its lossless session).
pub fn build_rctl(our_ssrc: u32, clock_ms: u16, frames: u16, highest_seq: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.push(0x80); // V=2, P=0, subtype=0
    out.push(PT_APP);
    out.extend_from_slice(&[0, 0]); // length, patched below
    out.extend_from_slice(&our_ssrc.to_be_bytes());
    out.extend_from_slice(b"RCTL");
    // 20-byte body.
    out.extend_from_slice(&[0x85, 0x00, 0x00, 0x04]);
    out.extend_from_slice(&frames.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // [6..12]
    out.extend_from_slice(&clock_ms.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // [14..16]
    out.extend_from_slice(&highest_seq.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // [18..20]
    patch_length(&mut out, 0);
    out
}

/// A compound RTCP report for liveness: Receiver Report + SDES(CNAME). Send this
/// periodically so the device knows the receiver is still alive.
pub fn build_liveness(our_ssrc: u32, cname: &str, blocks: &[ReportBlock]) -> Vec<u8> {
    let mut p = build_receiver_report(our_ssrc, blocks);
    p.extend_from_slice(&build_sdes(our_ssrc, cname));
    p
}

/// A compound RTCP keyframe request: RR + SDES + PLI + FIR. We send both PLI and
/// FIR because which one the AVConference encoder honors isn't guaranteed; both
/// are cheap and an encoder ignores the form it doesn't implement. `fir_seq` must
/// increment per request (see [`build_fir`]).
pub fn build_keyframe_request(
    our_ssrc: u32,
    cname: &str,
    media_ssrc: u32,
    blocks: &[ReportBlock],
    fir_seq: u8,
) -> Vec<u8> {
    let mut p = build_liveness(our_ssrc, cname, blocks);
    p.extend_from_slice(&build_pli(our_ssrc, media_ssrc));
    p.extend_from_slice(&build_fir(our_ssrc, media_ssrc, fir_seq));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every RTCP sub-packet's length field must equal (bytes/4 - 1), and the
    /// total length must be a multiple of 4.
    fn assert_well_formed(buf: &[u8]) {
        assert_eq!(buf.len() % 4, 0, "RTCP packet not 32-bit aligned");
        let mut off = 0;
        while off < buf.len() {
            assert!(off + 4 <= buf.len(), "truncated sub-packet header");
            assert_eq!(buf[off] >> 6, 2, "version must be 2");
            let len_words = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
            let pkt_len = (len_words + 1) * 4;
            assert!(
                off + pkt_len <= buf.len(),
                "sub-packet length overruns buffer"
            );
            off += pkt_len;
        }
        assert_eq!(off, buf.len(), "sub-packet lengths don't tile the buffer");
    }

    #[test]
    fn is_rtcp_discriminates_from_rtp() {
        // RTP video (PT 100), marker clear and set.
        assert!(!is_rtcp(&[0x80, 100, 0, 0]));
        assert!(!is_rtcp(&[0x80, 0x80 | 100, 0, 0]));
        // RTCP SR / RR / PSFB.
        assert!(is_rtcp(&[0x80, PT_SENDER_REPORT, 0, 0]));
        assert!(is_rtcp(&[0x81, PT_RECEIVER_REPORT, 0, 0]));
        assert!(is_rtcp(&[0x81, PT_PSFB, 0, 0]));
    }

    #[test]
    fn receiver_report_is_well_formed() {
        let blocks = [ReportBlock {
            source_ssrc: 0xdead_beef,
            fraction_lost: 12,
            cumulative_lost: 0x01_2345,
            highest_seq: 0x0001_8000,
            jitter: 42,
            lsr: 0xaabb_ccdd,
            dlsr: 0x0000_1000,
        }];
        let rr = build_receiver_report(0x1234_5678, &blocks);
        assert_well_formed(&rr);
        assert_eq!(rr[0] & 0x1f, 1, "RC should be 1");
        assert_eq!(rr[1], PT_RECEIVER_REPORT);
        // Layout: header(4) | our_ssrc(4) | block{ source_ssrc(4) @8 |
        // fraction_lost(1) @12 | cumulative_lost(3) @13 | ... }.
        assert_eq!(&rr[8..12], &0xdead_beefu32.to_be_bytes());
        assert_eq!(rr[12], 12, "fraction lost");
        assert_eq!(&rr[13..16], &[0x01, 0x23, 0x45], "cumulative loss (24-bit)");
    }

    #[test]
    fn liveness_and_keyframe_request_are_well_formed() {
        let blocks = [ReportBlock {
            source_ssrc: 1,
            ..Default::default()
        }];
        assert_well_formed(&build_liveness(7, "host@1.2.3.4", &blocks));
        assert_well_formed(&build_keyframe_request(
            7,
            "host@1.2.3.4",
            0xabcd,
            &blocks,
            3,
        ));
        // No report blocks (before we've seen any RTP) is still valid.
        assert_well_formed(&build_liveness(7, "h", &[]));
    }

    #[test]
    fn pli_and_fir_carry_the_right_ssrcs() {
        let pli = build_pli(0x1111_1111, 0x2222_2222);
        assert_well_formed(&pli);
        assert_eq!(pli[0] & 0x1f, FMT_PLI);
        assert_eq!(&pli[4..8], &0x1111_1111u32.to_be_bytes());
        assert_eq!(&pli[8..12], &0x2222_2222u32.to_be_bytes());

        let fir = build_fir(0x1111_1111, 0x2222_2222, 9);
        assert_well_formed(&fir);
        assert_eq!(fir[0] & 0x1f, FMT_FIR);
        assert_eq!(&fir[4..8], &0x1111_1111u32.to_be_bytes());
        assert_eq!(
            &fir[8..12],
            &0u32.to_be_bytes(),
            "FIR media-source SSRC must be 0"
        );
        assert_eq!(
            &fir[12..16],
            &0x2222_2222u32.to_be_bytes(),
            "FCI target SSRC"
        );
        assert_eq!(fir[16], 9, "FIR seq nr");
    }

    #[test]
    fn rctl_is_well_formed_and_carries_fields() {
        let r = build_rctl(0x00db_16eb, 12438, 6, 21);
        assert_well_formed(&r);
        assert_eq!(r.len(), 32);
        assert_eq!(r[1], PT_APP);
        assert_eq!(&r[8..12], b"RCTL");
        assert_eq!(&r[12..16], &[0x85, 0x00, 0x00, 0x04]);
        assert_eq!(&r[16..18], &6u16.to_be_bytes()); // frame counter
        assert_eq!(&r[24..26], &12438u16.to_be_bytes()); // clock at body[12..14]
        assert_eq!(&r[28..30], &21u16.to_be_bytes()); // highest seq at body[16..18]
    }

    #[test]
    fn frame_ack_matches_capture() {
        // Captured from Apple's Device Hub: 80cc0003 <ssrc> 00000005 <rtp ts>.
        let ack = build_frame_ack(0x00db_16eb, 5600);
        assert_well_formed(&ack);
        assert_eq!(ack.len(), 16);
        assert_eq!(
            &ack,
            &[
                0x80, 0xcc, 0x00, 0x03, 0x00, 0xdb, 0x16, 0xeb, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00,
                0x15, 0xe0, // 5600
            ]
        );
    }

    #[test]
    fn parses_first_sender_report() {
        // Build an SR by hand: header + ssrc + ntp(8) + rtp ts(4) + counts(8).
        let mut sr = vec![0x80, PT_SENDER_REPORT, 0, 6];
        sr.extend_from_slice(&0x0a0b_0c0du32.to_be_bytes()); // ssrc
        sr.extend_from_slice(&0x1111_2222u32.to_be_bytes()); // ntp msw
        sr.extend_from_slice(&0x3333_4444u32.to_be_bytes()); // ntp lsw
        sr.extend_from_slice(&0u32.to_be_bytes()); // rtp ts
        sr.extend_from_slice(&0u32.to_be_bytes()); // packet count
        sr.extend_from_slice(&0u32.to_be_bytes()); // octet count
        // Prepend an unrelated RR to prove we walk the compound packet.
        let mut compound = build_receiver_report(0xfeed, &[]);
        compound.extend_from_slice(&sr);

        let parsed = SenderReport::parse_first(&compound).expect("should find the SR");
        assert_eq!(parsed.ssrc, 0x0a0b_0c0d);
        // Middle 32 bits = low16(msw) << 16 | high16(lsw).
        assert_eq!(parsed.ntp_middle, 0x2222_3333);
    }
}
