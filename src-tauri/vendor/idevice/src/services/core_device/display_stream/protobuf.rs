// Jackson Coxson
//
// Minimal protobuf wire-format encoder/decoder.
//
// The AVConference media-negotiation blob (`VCMediaNegotiationBlobV2`) is an
// Apple protobuf message. The schema is tiny and fixed (see
// `media_negotiation.proto`), so rather than pull in `prost`/`protoc` we
// hand-roll just the bits of the wire format we need.
//
// Wire types we handle: 0 = varint, 2 = length-delimited (string/bytes/message).

/// Protobuf wire types.
pub const WIRE_VARINT: u32 = 0;
pub const WIRE_LEN: u32 = 2;

/// A growable protobuf encoder.
#[derive(Debug, Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn write_varint(&mut self, mut v: u64) {
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            self.buf.push(byte);
            if v == 0 {
                break;
            }
        }
    }

    fn write_tag(&mut self, field: u32, wire: u32) {
        self.write_varint(((field << 3) | wire) as u64);
    }

    /// Write a varint scalar field (uint32/uint64/bool/enum).
    pub fn uint_field(&mut self, field: u32, v: u64) {
        self.write_tag(field, WIRE_VARINT);
        self.write_varint(v);
    }

    /// Write a length-delimited field (string/bytes).
    pub fn bytes_field(&mut self, field: u32, v: &[u8]) {
        self.write_tag(field, WIRE_LEN);
        self.write_varint(v.len() as u64);
        self.buf.extend_from_slice(v);
    }

    pub fn string_field(&mut self, field: u32, v: &str) {
        self.bytes_field(field, v.as_bytes());
    }

    /// Write a nested message field by encoding `f` into a sub-encoder.
    pub fn message_field(&mut self, field: u32, f: impl FnOnce(&mut Encoder)) {
        let mut sub = Encoder::new();
        f(&mut sub);
        let bytes = sub.into_bytes();
        self.bytes_field(field, &bytes);
    }
}

/// A protobuf decoder over a borrowed buffer.
#[derive(Debug)]
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// One decoded protobuf field.
#[derive(Debug)]
pub enum Field<'a> {
    Varint(u64),
    Len(&'a [u8]),
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = *self.buf.get(self.pos)?;
            self.pos += 1;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    /// Read the next `(field_number, value)`. Returns `None` at end of buffer or
    /// on malformed input.
    pub fn next_field(&mut self) -> Option<(u32, Field<'a>)> {
        if self.is_empty() {
            return None;
        }
        let tag = self.read_varint()?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u32;
        match wire {
            WIRE_VARINT => Some((field, Field::Varint(self.read_varint()?))),
            WIRE_LEN => {
                let len = self.read_varint()? as usize;
                let end = self.pos.checked_add(len)?;
                let slice = self.buf.get(self.pos..end)?;
                self.pos = end;
                Some((field, Field::Len(slice)))
            }
            // 64-bit (1) and 32-bit (5): skip fixed widths so we stay aligned.
            1 => {
                self.pos = self.pos.checked_add(8)?;
                if self.pos > self.buf.len() {
                    return None;
                }
                self.next_field()
            }
            5 => {
                self.pos = self.pos.checked_add(4)?;
                if self.pos > self.buf.len() {
                    return None;
                }
                self.next_field()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Field<'_> {
        pub fn as_varint(&self) -> Option<u64> {
            match self {
                Field::Varint(v) => Some(*v),
                _ => None,
            }
        }

        pub fn as_bytes(&self) -> Option<&[u8]> {
            match self {
                Field::Len(b) => Some(b),
                _ => None,
            }
        }
    }

    #[test]
    fn roundtrip_scalars_and_message() {
        let mut e = Encoder::new();
        e.uint_field(1, 100);
        e.string_field(2, "abc");
        e.message_field(7, |m| {
            m.uint_field(1, 4);
            m.bytes_field(2, &[0xde, 0xad]);
        });
        let bytes = e.into_bytes();

        let mut d = Decoder::new(&bytes);
        let (f1, v1) = d.next_field().unwrap();
        assert_eq!(f1, 1);
        assert_eq!(v1.as_varint(), Some(100));
        let (f2, v2) = d.next_field().unwrap();
        assert_eq!(f2, 2);
        assert_eq!(v2.as_bytes(), Some(&b"abc"[..]));
        let (f7, v7) = d.next_field().unwrap();
        assert_eq!(f7, 7);
        let mut sub = Decoder::new(v7.as_bytes().unwrap());
        assert_eq!(sub.next_field().unwrap().1.as_varint(), Some(4));
        assert_eq!(
            sub.next_field().unwrap().1.as_bytes(),
            Some(&[0xde, 0xad][..])
        );
        assert!(d.next_field().is_none());
    }
}
