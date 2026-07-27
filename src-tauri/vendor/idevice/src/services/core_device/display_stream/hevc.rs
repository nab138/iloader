// Jackson Coxson
//
// HEVC RTP depacketization (RFC 7798) for the CoreDevice display stream.
//
// The device sends HEVC (H.265) over plaintext RTP, dynamic payload type 100.
//
// We reorder packets by RTP sequence number (the transport is UDP, so they can
// arrive out of order), reassemble NAL units, and emit them in Annex-B framing

const HEVC_NAL_HEADER_LEN: usize = 2;

const NAL_TYPE_AP: u8 = 48;
const NAL_TYPE_FU: u8 = 49;

// Parameter-set NAL unit types.
const NAL_TYPE_VPS: u8 = 32;
const NAL_TYPE_SPS: u8 = 33;
const NAL_TYPE_PPS: u8 = 34;

const NAL_TYPE_IRAP_LOW: u8 = 16;
const NAL_TYPE_IRAP_HIGH: u8 = 23;

/// Highest VCL NAL unit type (coded slice segments occupy 0..=31).
const NAL_TYPE_VCL_HIGH: u8 = 31;

const AUD_NAL: [u8; 3] = [0x46, 0x01, 0x50];
const ANNEXB_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];
const MAX_REORDER_BUFFER: usize = 128;

#[inline]
fn nal_type(nal_header_byte0: u8) -> u8 {
    (nal_header_byte0 >> 1) & 0x3f
}

#[inline]
fn is_irap(t: u8) -> bool {
    (NAL_TYPE_IRAP_LOW..=NAL_TYPE_IRAP_HIGH).contains(&t)
}

#[inline]
fn is_vcl(t: u8) -> bool {
    t <= NAL_TYPE_VCL_HIGH
}

/// Reassembles HEVC NAL units from RTP payloads and emits an Annex-B stream.
#[derive(Debug, Default)]
pub struct HevcDepacketizer {
    reorder: std::collections::BTreeMap<u16, (u32, Vec<u8>)>,
    next_seq: Option<u16>,
    last_ts: Option<u32>,

    fu_buffer: Vec<u8>,
    fu_active: bool,

    vps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    params_emitted_since_irap: bool,

    out: Vec<u8>,
}

impl HevcDepacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, seq: u16, timestamp: u32, payload: &[u8]) {
        if self.next_seq.is_none() {
            self.next_seq = Some(seq);
        }

        // Ignore packets we've already moved past (stale duplicates / late
        // arrivals behind the cursor).
        if let Some(next) = self.next_seq
            && seq_less_than(seq, next)
        {
            return;
        }

        self.reorder.insert(seq, (timestamp, payload.to_vec()));
        self.drain_in_order();

        // If we're stalled waiting on a lost packet, skip the gap.
        if self.reorder.len() > MAX_REORDER_BUFFER
            && let Some((&lowest, _)) = self.reorder.iter().next()
        {
            // A lost packet breaks any in-flight fragment.
            self.reset_fu();
            self.next_seq = Some(lowest);
            self.drain_in_order();
        }
    }

    /// Process buffered packets while they are contiguous from the cursor.
    fn drain_in_order(&mut self) {
        while let Some(next) = self.next_seq {
            let Some((ts, payload)) = self.reorder.remove(&next) else {
                break;
            };
            // A timestamp change marks a new access unit (frame). Close the
            // previous picture by emitting an AUD before the new one's NALs, so
            // the decoder doesn't merge slices from different frames together.
            if let Some(prev) = self.last_ts
                && prev != ts
            {
                self.write_annexb(&AUD_NAL);
            }
            self.last_ts = Some(ts);
            self.process_payload(&payload);
            self.next_seq = Some(next.wrapping_add(1));
        }
    }

    /// Handle a single RTP payload according to its NAL structure.
    fn process_payload(&mut self, payload: &[u8]) {
        if payload.len() < HEVC_NAL_HEADER_LEN {
            return;
        }
        match nal_type(payload[0]) {
            NAL_TYPE_AP => self.process_aggregation(payload),
            NAL_TYPE_FU => self.process_fragmentation(payload),
            _ => {
                // A new single NAL implies any in-flight fragment was lost.
                self.reset_fu();
                self.emit_nal(payload);
            }
        }
    }

    /// AP (type 48): `[NAL hdr][ (16-bit size)(NAL unit) ]+`. We do not negotiate
    /// `sprop-max-don-diff`, so there is no DONL/DOND field to skip.
    fn process_aggregation(&mut self, payload: &[u8]) {
        self.reset_fu();
        let mut off = HEVC_NAL_HEADER_LEN; // skip the AP's own 2-byte header
        while off + 2 <= payload.len() {
            let size = u16::from_be_bytes([payload[off], payload[off + 1]]) as usize;
            off += 2;
            let Some(nal) = payload.get(off..off + size) else {
                break; // truncated / malformed
            };
            self.emit_nal(nal);
            off += size;
        }
    }

    /// FU (type 49): a 2-byte FU NAL header, then a 1-byte FU header
    /// `[S|E|FuType(6)]`, then a fragment of the original NAL's payload. The
    /// original NAL header is reconstructed from the FU NAL header (layers/TID)
    /// with the type field replaced by `FuType`.
    fn process_fragmentation(&mut self, payload: &[u8]) {
        if payload.len() < HEVC_NAL_HEADER_LEN + 1 {
            return;
        }
        let fu_header = payload[2];
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let fu_type = fu_header & 0x3f;
        let fragment = &payload[3..];

        if start {
            // Reconstruct the original NAL header: take the FU NAL header's two
            // bytes and replace the type field (bits 1..=6 of byte 0) with the
            // FU type.
            let b0 = (payload[0] & 0x81) | (fu_type << 1);
            let b1 = payload[1];
            self.fu_buffer.clear();
            self.fu_buffer.push(b0);
            self.fu_buffer.push(b1);
            self.fu_buffer.extend_from_slice(fragment);
            self.fu_active = true;
        } else if self.fu_active {
            self.fu_buffer.extend_from_slice(fragment);
        } else {
            // Middle/end fragment with no start — the start packet was lost.
            return;
        }

        if end && self.fu_active {
            let nal = std::mem::take(&mut self.fu_buffer);
            self.fu_active = false;
            self.emit_nal(&nal);
        }
    }

    /// Append one complete NAL unit to the output, caching parameter sets and
    /// re-injecting them before key frames so a decoder can join mid-stream.
    fn emit_nal(&mut self, nal: &[u8]) {
        if nal.len() < HEVC_NAL_HEADER_LEN {
            return;
        }
        let t = nal_type(nal[0]);

        match t {
            NAL_TYPE_VPS => {
                self.vps = Some(nal.to_vec());
                self.params_emitted_since_irap = true;
            }
            NAL_TYPE_SPS => {
                self.sps = Some(nal.to_vec());
                self.params_emitted_since_irap = true;
            }
            NAL_TYPE_PPS => {
                self.pps = Some(nal.to_vec());
                self.params_emitted_since_irap = true;
            }
            _ => {}
        }

        // Before an IRAP (key) frame, make sure parameter sets precede it — but
        // only ONCE per key frame. A complex picture is coded as multiple slices,
        // i.e. several IRAP NAL units in a row for the *same* picture. Re-injecting
        // VPS/SPS/PPS before each slice would plant parameter sets *between* slices
        // of one picture, which a decoder reads as an access-unit boundary (H.265
        // AU detection treats a parameter set following a VCL NAL as the start of a
        // new AU). That splits one picture into several partial pictures: only the
        // first slice's CTUs decode and the rest of the frame is left stale. So we
        // inject only when params weren't already emitted for this key frame, then
        // mark them emitted so the remaining slices don't repeat it.
        if is_irap(t) && !self.params_emitted_since_irap {
            let sets: Vec<Vec<u8>> = [self.vps.clone(), self.sps.clone(), self.pps.clone()]
                .into_iter()
                .flatten()
                .collect();
            for set in sets {
                self.write_annexb(&set);
            }
            self.params_emitted_since_irap = true;
        }

        // Re-arm injection only when a *non-IRAP* VCL NAL (a P/B slice) goes by:
        // that marks the end of the key frame, so the next IRAP is a fresh key
        // frame needing its parameter sets again. Crucially, further slices of the
        // *same* IRAP picture (still IRAP NALs) and non-VCL NALs (SEI/AUD) must NOT
        // re-arm it, or we'd reintroduce the mid-picture injection above.
        if is_vcl(t) && !is_irap(t) {
            self.params_emitted_since_irap = false;
        }

        self.write_annexb(nal);
    }

    fn write_annexb(&mut self, nal: &[u8]) {
        self.out.extend_from_slice(&ANNEXB_START_CODE);
        self.out.extend_from_slice(nal);
    }

    fn reset_fu(&mut self) {
        self.fu_buffer.clear();
        self.fu_active = false;
    }

    /// Drain and return all Annex-B output accumulated so far.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// True once all three parameter sets (VPS/SPS/PPS) have been observed.
    pub fn has_parameter_sets(&self) -> bool {
        self.vps.is_some() && self.sps.is_some() && self.pps.is_some()
    }
}

/// RTP sequence-number comparison with 16-bit wraparound (RFC 1982). Returns
/// true if `a` is "before" `b` in sequence order.
#[inline]
fn seq_less_than(a: u16, b: u16) -> bool {
    let diff = b.wrapping_sub(a);
    diff != 0 && diff < 0x8000
}
