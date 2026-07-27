//! Read/write the device pasteboard via the
//! `com.apple.coredevice.pasteboardservice` RemoteXPC service.
//!
//! Unlike the DDI features, the service speaks XPC dicts directly: the
//! `command` field drives dispatch. Six verbs exist (request -> reply):
//!
//! * `PULL`  -> `PULL_REPLY`  read the pasteboard
//! * `SET`   -> `SET_REPLY`   replace the pasteboard
//! * `RESOLVE` -> `DATA`      fetch the bytes of a promised item
//! * `AUTONOTIFY` + `PUSH`    subscribe to change notifications (clipboard sync)
//!
//! A `PasteboardItem` is `{types: [UTI], data: {UTI: PasteboardItemData}}`. A
//! `PasteboardItemData` encodes as `{data: <bytes>}` when its bytes are inline,
//! `{size: <Int64>}` when the item is *promised* (fetch later with `RESOLVE`),
//! or `{error: ...}` when resolution failed. These map to
//! [`PasteboardPayload`].

use std::borrow::Cow;

use crate::{
    IdeviceError, ReadWrite, RemoteXpcClient, RsdService, obf, xpc,
    xpc::{Dictionary, XPCObject},
};

use super::CoreDeviceError;

pub const GENERAL_PASTEBOARD: &str = "general";

const UTI_UTF8_PLAIN_TEXT: &str = "public.utf8-plain-text";
const UTI_PLAIN_TEXT: &str = "public.plain-text";
const UTI_TEXT: &str = "public.text";

const TEXT_UTIS: [&str; 3] = [UTI_UTF8_PLAIN_TEXT, UTI_PLAIN_TEXT, UTI_TEXT];

/// Image UTIs we recognise on the pasteboard, in priority order. PNG first
/// (lossless, what we write back), then JPEG and TIFF.
pub const UTI_PNG: &str = "public.png";
pub const UTI_JPEG: &str = "public.jpeg";
pub const UTI_TIFF: &str = "public.tiff";

const IMAGE_UTIS: [&str; 3] = [UTI_PNG, UTI_JPEG, UTI_TIFF];

/// `PasteboardDataInclusionPolicy` tells the device whether to inline item
/// bytes in a `PULL`/`PUSH` snapshot or hand them back as promises to fetch
/// with `RESOLVE`.
#[derive(Debug, Clone, Copy)]
pub enum DataInclusionPolicy {
    /// Resolve and inline every item's bytes. Simplest for callers; can be
    /// expensive for large items.
    AllResolved,
    /// Promise every item; fetch bytes on demand with [`resolve`](PasteboardServiceClient::resolve).
    AllPromised,
    /// Mirror whatever the source set on the device.
    MatchSource,
    /// Resolve the primary item, promise the rest.
    PromiseSecondary,
    /// Inline items smaller than `bytes`, promise the rest.
    Threshold(i64),
}

impl DataInclusionPolicy {
    fn to_xpc(self) -> XPCObject {
        let (key, value) = match self {
            DataInclusionPolicy::AllResolved => ("allResolved", Dictionary::new()),
            DataInclusionPolicy::AllPromised => ("allPromised", Dictionary::new()),
            DataInclusionPolicy::MatchSource => ("matchSource", Dictionary::new()),
            DataInclusionPolicy::PromiseSecondary => ("promiseSecondary", Dictionary::new()),
            DataInclusionPolicy::Threshold(bytes) => {
                let mut inner = Dictionary::new();
                inner.insert("bytes".into(), XPCObject::Int64(bytes));
                ("thresholdData", inner)
            }
        };
        let mut d = Dictionary::new();
        d.insert(key.into(), XPCObject::Dictionary(value));
        XPCObject::Dictionary(d)
    }
}

/// The bytes (or absence thereof) for one UTI of a [`PasteboardItem`].
#[derive(Debug, Clone)]
pub enum PasteboardPayload {
    /// The bytes were inlined in the snapshot.
    Inline(Vec<u8>),
    /// The item is promised: fetch with [`PasteboardServiceClient::resolve`].
    /// `size` is the device's advertised byte count when known.
    Promised { size: Option<i64> },
    /// The device reported an error resolving this UTI, rendered for diagnostics.
    Error(String),
}

/// One UTI's payload within a [`PasteboardItem`].
#[derive(Debug, Clone)]
pub struct PasteboardEntry {
    pub uti: String,
    pub payload: PasteboardPayload,
}

/// A single pasteboard item: its declared UTIs (in priority order) and the
/// payload for each.
#[derive(Debug, Clone)]
pub struct PasteboardItem {
    /// Position in the snapshot — the `item_index` to pass to
    /// [`PasteboardServiceClient::resolve`].
    pub index: usize,
    /// Declared UTIs in priority order.
    pub types: Vec<String>,
    /// Per-UTI payloads.
    pub data: Vec<PasteboardEntry>,
}

/// A parsed `PULL_REPLY`/`PUSH` pasteboard snapshot.
#[derive(Debug, Clone)]
pub struct PasteboardSnapshot {
    /// The pasteboard the snapshot is for, e.g. [`GENERAL_PASTEBOARD`].
    pub pasteboard_name: Option<String>,
    /// Monotonic change counter; bumps on every pasteboard mutation.
    pub change_count: Option<i64>,
    pub items: Vec<PasteboardItem>,
}

/// A promised (not-inlined) `(item, UTI)` pair to fetch with
/// [`PasteboardServiceClient::resolve`].
#[derive(Debug, Clone)]
pub struct PromisedItem {
    pub item_index: i64,
    pub uti: String,
    pub size: Option<i64>,
}

impl PasteboardSnapshot {
    /// Parse a `PULL_REPLY`/`PUSH` message. Both nest the snapshot under a
    /// `pasteboard` key; a bare snapshot dict is also accepted.
    fn from_message(msg: &plist::Value) -> Result<Self, CoreDeviceError> {
        let dict = msg
            .as_dictionary()
            .ok_or(CoreDeviceError::MalformedField("(root)"))?;
        // PULL_REPLY/PUSH nest the snapshot under "pasteboard"; tolerate a bare
        // snapshot too.
        let snapshot = dict
            .get("pasteboard")
            .and_then(|p| p.as_dictionary())
            .unwrap_or(dict);

        let metadata = snapshot.get("metadata").and_then(|m| m.as_dictionary());
        let pasteboard_name = metadata
            .and_then(|m| m.get("pasteboardName"))
            .and_then(|n| n.as_string())
            .map(str::to_string);
        let change_count = metadata
            .and_then(|m| m.get("changeCount"))
            .and_then(|c| c.as_signed_integer());

        let items = snapshot
            .get("items")
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .map(|(index, item)| parse_item(index, item))
                    .collect()
            })
            .unwrap_or_default();

        Ok(PasteboardSnapshot {
            pasteboard_name,
            change_count,
            items,
        })
    }

    /// Best-effort UTF-8 text: the first text UTI carrying inline bytes.
    pub fn text(&self) -> Option<String> {
        for item in &self.items {
            for uti in TEXT_UTIS {
                if let Some(entry) = item.data.iter().find(|e| e.uti == uti)
                    && let PasteboardPayload::Inline(bytes) = &entry.payload
                    && let Ok(text) = String::from_utf8(bytes.clone())
                {
                    return Some(text);
                }
            }
        }
        None
    }

    /// Best-effort image bytes: the first image UTI (PNG/JPEG/TIFF, in priority
    /// order) carrying inline bytes, returned with the UTI that produced them so
    /// the caller knows the encoding. Promised items are skipped — pull with
    /// [`DataInclusionPolicy::AllResolved`] (the default for [`get`](PasteboardServiceClient::get))
    /// to inline them, or fetch with [`resolve`](PasteboardServiceClient::resolve).
    pub fn image(&self) -> Option<(&'static str, Vec<u8>)> {
        for item in &self.items {
            for uti in IMAGE_UTIS {
                if let Some(entry) = item.data.iter().find(|e| e.uti == uti)
                    && let PasteboardPayload::Inline(bytes) = &entry.payload
                {
                    return Some((uti, bytes.clone()));
                }
            }
        }
        None
    }

    /// The promised (not-inlined) entries, each fetchable with
    /// [`PasteboardServiceClient::resolve`].
    pub fn promised_items(&self) -> Vec<PromisedItem> {
        let mut out = Vec::new();
        for item in &self.items {
            for entry in &item.data {
                if let PasteboardPayload::Promised { size } = entry.payload {
                    out.push(PromisedItem {
                        item_index: item.index as i64,
                        uti: entry.uti.clone(),
                        size,
                    });
                }
            }
        }
        out
    }
}

fn parse_item(index: usize, item: &plist::Value) -> PasteboardItem {
    let dict = item.as_dictionary();
    let types = dict
        .and_then(|d| d.get("types"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_string().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let data = dict
        .and_then(|d| d.get("data"))
        .and_then(|d| d.as_dictionary())
        .map(|map| {
            map.iter()
                .map(|(uti, datum)| PasteboardEntry {
                    uti: uti.clone(),
                    payload: parse_payload(datum),
                })
                .collect()
        })
        .unwrap_or_default();

    PasteboardItem { index, types, data }
}

fn parse_payload(datum: &plist::Value) -> PasteboardPayload {
    let Some(d) = datum.as_dictionary() else {
        return PasteboardPayload::Promised { size: None };
    };
    if let Some(bytes) = d.get("data").and_then(|v| v.as_data()) {
        PasteboardPayload::Inline(bytes.to_vec())
    } else if let Some(err) = d.get("error") {
        PasteboardPayload::Error(format!("{err:?}"))
    } else {
        PasteboardPayload::Promised {
            size: d.get("size").and_then(|s| s.as_signed_integer()),
        }
    }
}

#[derive(Debug)]
pub struct PasteboardServiceClient<R: ReadWrite> {
    inner: RemoteXpcClient<R>,
}

impl RsdService for PasteboardServiceClient<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> Cow<'static, str> {
        obf!("com.apple.coredevice.pasteboardservice")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        let mut inner = RemoteXpcClient::new(stream).await?;
        inner.do_handshake().await?;
        Ok(Self { inner })
    }
}

impl<R: ReadWrite> PasteboardServiceClient<R> {
    pub fn new(inner: RemoteXpcClient<R>) -> Self {
        Self { inner }
    }

    /// Pull `pasteboard_name` with every item's bytes inlined
    /// ([`DataInclusionPolicy::AllResolved`]).
    pub async fn get(&mut self, pasteboard_name: &str) -> Result<PasteboardSnapshot, IdeviceError> {
        self.get_with_policy(pasteboard_name, DataInclusionPolicy::AllResolved)
            .await
    }

    /// Pull `pasteboard_name` with an explicit data-inclusion `policy`. Use a
    /// promising policy plus [`PasteboardSnapshot::promised_items`] +
    /// [`resolve`](Self::resolve) to avoid transferring large items you don't
    /// need.
    pub async fn get_with_policy(
        &mut self,
        pasteboard_name: &str,
        policy: DataInclusionPolicy,
    ) -> Result<PasteboardSnapshot, IdeviceError> {
        let msg = xpc!({
            "command": "PULL",
            "pasteboardName": pasteboard_name,
            "dataPolicy": policy.to_xpc(),
        });

        self.inner.send_object(msg, true).await?;
        let reply = self.inner.recv().await?;
        Ok(PasteboardSnapshot::from_message(&reply)?)
    }

    /// Replace the contents of `pasteboard_name` with a single UTF-8 `text` value.
    pub async fn set_text(
        &mut self,
        text: &str,
        pasteboard_name: &str,
    ) -> Result<(), IdeviceError> {
        let mut data = Dictionary::new();
        for uti in TEXT_UTIS {
            data.insert(uti.into(), xpc!({ "data": text.as_bytes() }));
        }

        let msg = xpc!({
            "command": "SET",
            "pasteboardName": pasteboard_name,
            "items": [{
                "types": TEXT_UTIS.to_vec(),
                "data": XPCObject::Dictionary(data),
            }],
            "sourceMetadata": XPCObject::Null,
        });

        self.inner.send_object(msg, true).await?;
        // Drain the SET_REPLY ack; it carries no information we surface.
        self.inner.recv().await?;
        Ok(())
    }

    /// Replace the contents of `pasteboard_name` with a single image item of the
    /// given `uti` (e.g. [`UTI_PNG`]) and its encoded `bytes`.
    pub async fn set_image(
        &mut self,
        bytes: &[u8],
        uti: &str,
        pasteboard_name: &str,
    ) -> Result<(), IdeviceError> {
        let mut data = Dictionary::new();
        data.insert(uti.into(), xpc!({ "data": bytes }));

        let msg = xpc!({
            "command": "SET",
            "pasteboardName": pasteboard_name,
            "items": [{
                "types": vec![uti],
                "data": XPCObject::Dictionary(data),
            }],
            "sourceMetadata": XPCObject::Null,
        });

        self.inner.send_object(msg, true).await?;
        // Drain the SET_REPLY ack; it carries no information we surface.
        self.inner.recv().await?;
        Ok(())
    }

    /// Fetch the bytes of a promised item (`RESOLVE` -> `DATA`). `item_index` is
    /// the item's position in the snapshot and `uti` is the type to resolve.
    ///
    /// Returns `None` when the device can't fulfil the promise — e.g. the
    /// snapshot was superseded by a newer pasteboard change, or the data is no
    /// longer available (the `DATA` reply carries a null `data`).
    pub async fn resolve(
        &mut self,
        pasteboard_name: &str,
        item_index: i64,
        uti: &str,
    ) -> Result<Option<Vec<u8>>, IdeviceError> {
        let reply = self.resolve_raw(pasteboard_name, item_index, uti).await?;
        Ok(reply
            .as_dictionary()
            .and_then(|d| d.get("data"))
            .and_then(|d| d.as_data())
            .map(|d| d.to_vec()))
    }

    /// Like [`resolve`](Self::resolve) but returns the raw `DATA` reply for
    /// callers that need to inspect it (e.g. an `error` instead of `data`).
    pub async fn resolve_raw(
        &mut self,
        pasteboard_name: &str,
        item_index: i64,
        uti: &str,
    ) -> Result<plist::Value, IdeviceError> {
        let msg = xpc!({
            "command": "RESOLVE",
            "pasteboardName": pasteboard_name,
            "itemIndex": item_index,
            "type": uti,
        });

        self.inner.send_object(msg, true).await?;
        self.inner.recv().await
    }

    /// Subscribe to (or, with `enable = false`, unsubscribe from) pasteboard
    /// change notifications. While subscribed the device pushes `PUSH` snapshots
    /// as the pasteboard changes; read them with [`recv_push`](Self::recv_push).
    ///
    /// `policy` controls whether the pushed snapshots carry inline bytes.
    ///
    /// The request is sent one-way: the device does not reply on the reply
    /// channel, it just begins pushing `PUSH` messages on the root channel.
    pub async fn set_change_notifications(
        &mut self,
        enable: bool,
        pasteboard_name: &str,
        policy: Option<DataInclusionPolicy>,
    ) -> Result<(), IdeviceError> {
        let data_policy = policy.map(|p| p.to_xpc()).unwrap_or(XPCObject::Null);
        let msg = xpc!({
            "command": "AUTONOTIFY",
            "enable": enable,
            "pasteboardName": pasteboard_name,
            "dataPolicy": data_policy,
        });

        self.inner.send_object(msg, false).await
    }

    /// Block for the next `PUSH` change-notification snapshot from the device.
    /// Only meaningful after [`set_change_notifications`](Self::set_change_notifications)
    /// with `enable = true`.
    ///
    /// Pushes arrive on the root channel (not the reply channel that
    /// request/reply verbs use), so this reads from there.
    pub async fn recv_push(&mut self) -> Result<PasteboardSnapshot, IdeviceError> {
        let push = self.inner.recv_root().await?;
        Ok(PasteboardSnapshot::from_message(&push)?)
    }
}
