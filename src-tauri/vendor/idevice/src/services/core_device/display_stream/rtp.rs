// Jackson Coxson
//
// RTP parsing for the CoreDevice display stream.
//
// The device sends a plaintext RTP stream (no SRTP, the negotiated
// SRTPCipherSuite is 0) to the receiver address/port we hand it in the
// `startvideooutput` request. Video is HEVC, dynamic payload type 100.
//
// This module only parses the RTP framing; HEVC access-unit reassembly and
// decoding build on top of `RtpPacket`.

/// A parsed RTP packet (RFC 3550), borrowing its payload from the input buffer.
#[derive(Debug, Clone)]
pub struct RtpPacket<'a> {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub csrc: Vec<u32>,
    /// Profile-specific extension header (id, data) if `extension` is set.
    pub ext_profile: u16,
    pub ext_data: &'a [u8],
    /// The media payload (after CSRC list, extension, and minus any padding).
    pub payload: &'a [u8],
}

impl<'a> RtpPacket<'a> {
    /// Parse an RTP packet from a UDP datagram. Returns `None` if malformed.
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 12 {
            return None;
        }
        let b0 = buf[0];
        let version = b0 >> 6;
        if version != 2 {
            return None;
        }
        let padding = b0 & 0x20 != 0;
        let extension = b0 & 0x10 != 0;
        let csrc_count = (b0 & 0x0f) as usize;

        let b1 = buf[1];
        let marker = b1 & 0x80 != 0;
        let payload_type = b1 & 0x7f;

        let sequence_number = u16::from_be_bytes([buf[2], buf[3]]);
        let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

        let mut off = 12;
        let mut csrc = Vec::with_capacity(csrc_count);
        for _ in 0..csrc_count {
            let end = off + 4;
            let w = buf.get(off..end)?;
            csrc.push(u32::from_be_bytes([w[0], w[1], w[2], w[3]]));
            off = end;
        }

        let mut ext_profile = 0u16;
        let mut ext_data: &[u8] = &[];
        if extension {
            let hdr = buf.get(off..off + 4)?;
            ext_profile = u16::from_be_bytes([hdr[0], hdr[1]]);
            let ext_words = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
            off += 4;
            let ext_len = ext_words * 4;
            ext_data = buf.get(off..off + ext_len)?;
            off += ext_len;
        }

        let mut end = buf.len();
        if padding {
            // Last byte is the padding length (including itself).
            let pad = *buf.last()? as usize;
            if pad == 0 || pad > end.saturating_sub(off) {
                return None;
            }
            end -= pad;
        }
        let payload = buf.get(off..end)?;

        Some(RtpPacket {
            version,
            padding,
            extension,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            csrc,
            ext_profile,
            ext_data,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_rtp() {
        // V=2, PT=100, marker set, seq=1, ts=2, ssrc=3, payload "hi"
        let mut pkt = vec![0x80, 0x80 | 100, 0x00, 0x01, 0, 0, 0, 2, 0, 0, 0, 3];
        pkt.extend_from_slice(b"hi");
        let p = RtpPacket::parse(&pkt).unwrap();
        assert_eq!(p.payload_type, 100);
        assert!(p.marker);
        assert_eq!(p.sequence_number, 1);
        assert_eq!(p.timestamp, 2);
        assert_eq!(p.ssrc, 3);
        assert_eq!(p.payload, b"hi");
    }

    #[test]
    fn parses_extension_header() {
        // extension bit set, 1 ext word
        let pkt = vec![
            0x90, 100, 0, 1, 0, 0, 0, 0, 0, 0, 0, 5, // header
            0xBE, 0xDE, 0, 1, // ext profile 0xBEDE, 1 word
            0xAA, 0xBB, 0xCC, 0xDD, // ext data
            0x01, 0x02, // payload
        ];
        let p = RtpPacket::parse(&pkt).unwrap();
        assert_eq!(p.ext_profile, 0xBEDE);
        assert_eq!(p.ext_data, &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(p.payload, &[0x01, 0x02]);
    }
}
