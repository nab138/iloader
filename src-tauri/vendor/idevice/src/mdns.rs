//! Bonjour/mDNS device matching.
//!
//! iOS 26.4+ advertises a paired host over Bonjour (`_apple-mobdev2._tcp`)
//! with an `identifier` TXT value and one or more `authTag` TXT values. A
//! muxer matches a service to a known device by recomputing the auth tag from
//! the device's `HostID` (read from its pairing record) and comparing it
//! against the advertised tags.
//!
//! This mirrors MobileDevice's `AMDIsTXTRecordForUDID`:
//! ```text
//! K        = HKDF-SHA512(ikm = HostID, salt = "", info = "", L = 32)
//! expected = HMAC-SHA256(K, identifier)[0..8]
//! match    = expected == base64_decode(authTag)[0..8] for some authTag
//! ```

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Sha256, Sha512};

/// Derives the 8-byte auth tag a device with `host_id` publishes for a given
/// Bonjour `identifier`.
///
/// `host_id` is the device's `HostID` (the UTF-8 bytes of the string stored in
/// its pairing record). `identifier` is the raw `identifier` TXT value.
pub fn derive_auth_tag(host_id: &[u8], identifier: &[u8]) -> [u8; 8] {
    let hk = Hkdf::<Sha512>::new(None, host_id);
    let mut key = [0u8; 32];
    // `expand` only fails when the output length exceeds 255 * HashLen; 32
    // bytes is always valid for SHA-512.
    hk.expand(&[], &mut key)
        .expect("32 is a valid HKDF-SHA512 output length");

    // HMAC accepts a key of any length, so this never errors.
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key)
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(identifier);
    let tag = mac.finalize().into_bytes();

    let mut out = [0u8; 8];
    out.copy_from_slice(&tag[..8]);
    out
}

/// Decodes an `authTag` TXT value to its 8-byte form.
///
/// Bonjour TXT values are raw bytes; the `authTag` entries carry base64-encoded
/// 8-byte HMAC truncations. ASCII whitespace is trimmed before decoding (as
/// MobileDevice does). Anything that doesn't decode to exactly 8 bytes returns
/// `None`.
pub fn decode_auth_tag(raw: &[u8]) -> Option<[u8; 8]> {
    let trimmed = raw
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|start| {
            let end = raw
                .iter()
                .rposition(|b| !b.is_ascii_whitespace())
                .map(|i| i + 1)
                .unwrap_or(raw.len());
            &raw[start..end]
        })
        .unwrap_or(&[][..]);
    let decoded = B64.decode(trimmed).ok()?;
    decoded.as_slice().try_into().ok()
}

/// Returns `true` if the device with `host_id` published any of `auth_tags`
/// for `identifier`.
///
/// `auth_tags` are the raw (base64) `authTag` TXT values; each is decoded and
/// compared against the tag derived from `host_id`.
pub fn txt_record_matches(host_id: &[u8], identifier: &[u8], auth_tags: &[&[u8]]) -> bool {
    let expected = derive_auth_tag(host_id, identifier);
    auth_tags
        .iter()
        .filter_map(|tag| decode_auth_tag(tag))
        .any(|tag| tag == expected)
}
