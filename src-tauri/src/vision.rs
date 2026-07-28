//! Apple Vision Pro over Wi-Fi — a parallel device path that does NOT go through
//! usbmux. A Wi-Fi Vision Pro is not carried by usbmuxd, so it can't be reached via
//! `UsbmuxdProvider` like an iPhone/iPad. Instead we:
//!
//!   1. discover it over mDNS (`_remotepairing-manual-pairing._tcp`) — via the
//!      system Bonjour daemon on macOS (see the `bonjour` module for why), via
//!      `mdns_sd`'s own multicast sockets elsewhere,
//!   2. first-time pair with the 6-digit code shown on the headset (`vision_pair`),
//!   3. reach its install services over an RP tunnel:
//!        pair-verify on RSD :49152 -> create_tcp_listener -> TLS-PSK CDTunnel
//!        -> software TCP stack (Adapter) -> RSD handshake -> RSD services.
//!
//! This is lifted from the standalone `vision-sideload` CLI (its `vision_net.rs`,
//! `install.rs`, `place.rs`, `pair.rs`), which was validated end-to-end against a
//! real Vision Pro. The iOS/iPad path in `device.rs` is unchanged.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use idevice::RsdService;
use idevice::afc::AfcClient;
use idevice::afc::opcode::AfcFopenMode;
use idevice::house_arrest::HouseArrestClient;
use idevice::installation_proxy::InstallationProxyClient;
use idevice::remote_pairing::{
    RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native,
};
use idevice::rsd::RsdHandshake;
use idevice::tcp::adapter::Adapter;
use idevice::tcp::handle::AdapterHandle;
#[cfg(not(target_os = "macos"))]
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use tokio::net::TcpStream;

use tauri::{AppHandle, Emitter, Listener, State, Window};
use tokio_util::sync::CancellationToken;

use crate::device::{
    DeviceInfo, DeviceInfoMutex, DeviceInfoWithPairing, DeviceTransport, PairingCancelToken,
};
use crate::error::AppError;
use crate::pairing::{delete_pairing_data, retrieve_pairing_data, store_pairing_data};

/// Manual-pairing service — INITIAL pairing only (pops the headset code). A Vision Pro
/// advertises this ONLY while it is NOT yet paired to us; once paired it stops.
pub const MANUAL_PAIRING_SERVICE: &str = "_remotepairing-manual-pairing._tcp.local.";
/// RemotePairing (RSD) service — a PAIRED Vision Pro advertises this (SRV target is the
/// device hostname on port 49152). Browsing it is how we keep listing a VP after it's
/// paired (so users can reinstall). iPhones advertise it too, so we filter by hostname.
pub const REMOTE_PAIRING_SERVICE: &str = "_remotepairing._tcp.local.";
/// The port pair-verify + tunnel creation live on (the VP's RSD). Stable across the
/// session; NOT the (dynamic) manual-pairing port and NOT 65000.
pub const RSD_PORT: u16 = 49152;

/// How long `vision_pair` waits for a live manual-pairing announcement (polls ×250ms).
/// Sized for the observed re-announcement cadence (~30-60s between announcements, with
/// the service absent in between), so a tap that lands in a gap still succeeds.
const PAIRABLE_WAIT_POLLS: usize = 160;

fn ap_err(context: &str, e: impl std::fmt::Debug) -> AppError {
    AppError::RemotePairing(format!("{context}: {e:?}"))
}

/// How fast an `EHOSTUNREACH` must arrive to count as the kernel REFUSING to send
/// rather than trying and giving up. A genuinely unreachable LAN peer fails only
/// after several ARP probes (seconds); an instant "No route to host" on a
/// directly-attached subnet is a policy verdict — on macOS 15+, the per-app Local
/// Network privacy filter, whose state is known to get stuck out of sync with the
/// Settings switch (notably after app or macOS updates). Field-confirmed: a user's
/// Mac where ping/nc from Terminal reached the headset fine (terminal processes are
/// exempt, TN3179) while iloader's connects failed instantly.
const INSTANT_UNREACHABLE: Duration = Duration::from_millis(300);

/// A TCP connect to the (already discovered) headset failed — explain the common
/// causes instead of dumping a bare OS error. `HostUnreachable`/`TimedOut` against a
/// LAN peer usually isn't routing: the headset is asleep (ARP goes unanswered), or
/// the Wi-Fi router keeps wireless clients from talking to each other (guest network
/// / AP or client isolation), or a VPN/firewall intercepts local traffic. When the
/// failure was INSTANT, it's macOS itself refusing — see [`INSTANT_UNREACHABLE`].
fn connect_err(what: &str, ip: &str, port: u16, e: std::io::Error) -> AppError {
    AppError::VisionUnreachable(connect_err_message(what, ip, port, e, None))
}

fn connect_err_message(
    what: &str,
    ip: &str,
    port: u16,
    e: std::io::Error,
    elapsed: Option<Duration>,
) -> String {
    use std::io::ErrorKind;
    let hint = match e.kind() {
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable
            if elapsed.is_some_and(|d| d < INSTANT_UNREACHABLE) =>
        {
            "\nmacOS refused this connection instantly instead of trying and timing out. \
             If the headset is awake and on this network, that usually means macOS's \
             Local Network permission for iloader is stuck — even with the switch ON. \
             In System Settings ▸ Privacy & Security ▸ Local Network, switch iloader \
             OFF and back ON, then quit and reopen iloader. If it still fails, restart \
             the Mac — this is a known macOS quirk after app or system updates."
        }
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable | ErrorKind::TimedOut => {
            "\nThe Vision Pro was found, but the Mac can't reach it directly. Usually this \
             means the headset is asleep (put it on and keep it awake), the Wi-Fi router \
             blocks devices from talking to each other (guest network or AP/client \
             isolation — try your main network), or a VPN/firewall is in the way."
        }
        ErrorKind::ConnectionRefused => {
            "\nThe Vision Pro refused the connection — its pairing service may have just \
             restarted with a new port. Try again in a few seconds."
        }
        _ => "",
    };
    format!("{what} ({ip}:{port}): {e}{hint}")
}

/// TCP-connect to the first address that answers.
///
/// A headset advertises several addresses and only some are routable from this Mac,
/// so trying just one turns a perfectly reachable device into "No route to host".
async fn connect_any(ips: &[String], port: u16, what: &str) -> Result<(TcpStream, String), AppError> {
    let mut last = None;
    for ip in ips {
        let started = std::time::Instant::now();
        match TcpStream::connect((ip.as_str(), port)).await {
            Ok(s) => {
                // Logged BEFORE any protocol byte is exchanged, so a later failure
                // can never masquerade as a connect failure in a user log — and the
                // local address shows which interface the kernel routed us out of.
                tracing::info!(
                    target: "vision",
                    "{what}: TCP connected to {ip}:{port} in {:?} (local {})",
                    started.elapsed(),
                    s.local_addr().map_or_else(|_| "?".into(), |a| a.to_string())
                );
                return Ok((s, ip.clone()));
            }
            Err(e) => {
                let elapsed = started.elapsed();
                // The timing is diagnostic gold in user logs: instant = macOS policy,
                // seconds = nobody answered ARP (asleep / isolated network).
                tracing::debug!(
                    target: "vision",
                    "{what}: {ip}:{port} unreachable after {elapsed:?} ({e})"
                );
                last = Some(connect_err_message(what, ip, port, e, Some(elapsed)));
            }
        }
    }
    Err(AppError::VisionUnreachable(last.unwrap_or_else(|| {
        format!("{what}: the Vision Pro advertised no usable address")
    })))
}

/// A Vision Pro discovered over mDNS.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub name: String,
    /// Preferred address (first entry of `ips`).
    pub ip: String,
    /// Every advertised IPv4, routable ones first. A headset often advertises more
    /// than one and only some are reachable from this Mac, so connections walk this
    /// list rather than trusting a single address.
    pub ips: Vec<String>,
    /// The manual-pairing port, present only when the manual-pairing service is being
    /// advertised (i.e. the device is not yet paired to us and CAN be paired now). A
    /// paired VP has `None` here — it's reached via the stored pairing over the RSD.
    pub manual_pairing_port: Option<u16>,
}

impl Discovered {
    /// A stable synthetic device id derived from the canonical name, kept in a high
    /// range so it never collides with usbmux's small integer device ids. Using the
    /// canonical form keeps the id stable across the manual-pairing (friendly name)
    /// and remotepairing (hostname) views of the same device.
    pub fn synthetic_id(&self) -> u32 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        canonical(&self.name).hash(&mut h);
        0x5100_0000u32 | (h.finish() as u32 & 0x00FF_FFFF)
    }
}

/// Canonical device identity: lowercase, alphanumerics only. This makes the two mDNS
/// views of the same Vision Pro agree — the manual-pairing service reports the friendly
/// name ("Sam's Apple Vision Pro") while the remotepairing service reports the
/// hostname ("Sams-AppleVisionPro"); both canonicalize to "samsapplevisionpro".
fn canonical(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The pairing-storage key under which a Vision Pro's RP pairing file is cached. Keyed
/// on the canonical name so a paired VP (discovered by hostname) resolves to the same
/// key it was stored under at pair time (discovered by friendly name).
pub fn pairing_storage_key(name: &str) -> String {
    format!("vision_pairing_{}", canonical(name))
}

/// Friendly instance name from a manual-pairing fullname
/// ("Sam's Apple Vision Pro._remotepairing-manual-pairing._tcp.local." → the name).
/// (mdns_sd only — dns_sd browse callbacks already carry the unescaped instance name.)
#[cfg(not(target_os = "macos"))]
fn instance_name(fullname: &str) -> String {
    fullname
        .split("._remotepairing")
        .next()
        .unwrap_or("Vision Pro")
        .replace('\u{a0}', " ")
        .to_string()
}

/// The device label from an mDNS hostname ("Sams-AppleVisionPro.local." →
/// "Sams AppleVisionPro"). Used for the remotepairing view, which carries only a
/// UUID instance name, not the friendly name.
fn hostname_label(hostname: &str) -> String {
    hostname
        .trim_end_matches('.')
        .strip_suffix(".local")
        .unwrap_or(hostname)
        .replace(['-', '_'], " ")
}

/// True if a hostname looks like a Vision Pro (so we don't list iPhones, which also
/// advertise `_remotepairing._tcp` and are handled over usbmux). Hostname shape
/// depends on the device name — a default-named headset is "Apple-Vision-Pro.local.",
/// a custom-named one e.g. "Sams-AppleVisionPro.local." — so compare on
/// alphanumerics only, or the hyphenated default slips through the filter.
fn is_vision_hostname(hostname: &str) -> bool {
    let h = canonical(hostname);
    h.contains("visionpro") || h.contains("realitydevice")
}

/// A persistent mDNS browser. One long-lived `ServiceDaemon` browses BOTH the
/// manual-pairing service (unpaired VPs — pops the code) and the remotepairing service
/// (already-paired VPs), for the app's whole lifetime, keeping the current Vision Pro
/// set up to date.
///
/// Persistent (not one-shot) browsing matters twice over: a cold one-shot browse races
/// the multicast round-trip and returns nothing on first launch, and — since a VP stops
/// advertising manual-pairing once paired — only continuous browsing of BOTH services
/// keeps a paired VP visible so users can reinstall.
struct VisionBrowser {
    devices: Arc<Mutex<HashMap<String, Discovered>>>,
    // Kept alive for the process lifetime; `None` if mDNS was unavailable, in which
    // case `devices` simply stays empty (no Vision Pros) rather than erroring. On
    // macOS the browse tasks are detached onto the async runtime instead.
    #[cfg(not(target_os = "macos"))]
    _daemon: Option<ServiceDaemon>,
}

impl VisionBrowser {
    fn snapshot(&self) -> Vec<Discovered> {
        self.devices.lock().unwrap().values().cloned().collect()
    }

    /// The named device if known, else any discovered device (single-VP convenience).
    fn get(&self, name: &str) -> Option<Discovered> {
        let key = canonical(name);
        let guard = self.devices.lock().unwrap();
        guard
            .get(&key)
            .cloned()
            .or_else(|| guard.values().next().cloned())
    }
}

static BROWSER: OnceLock<VisionBrowser> = OnceLock::new();

/// Human-readable reason Vision Pro discovery couldn't start — on macOS a rejected
/// Bonjour browse (most notably kDNSServiceErr_PolicyDenied when Local Network
/// permission is off), elsewhere an mDNS stack that failed to bind/join multicast.
/// `None` while discovery is healthy. Surfaced to the UI's empty-list hint via the
/// `vision_discovery_error` command. Previously this failure was swallowed
/// by `ServiceDaemon::new().ok()`, so a blocked mDNS looked identical to "no device".
static DISCOVERY_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn set_discovery_error(msg: Option<String>) {
    if let Ok(mut guard) = DISCOVERY_ERROR.lock() {
        *guard = msg;
    }
}

/// The current discovery-startup error, if any. `None` means discovery is running (a
/// device simply may not be present/reachable yet) — callers must not read `None` as
/// "a device exists".
pub fn discovery_error() -> Option<String> {
    DISCOVERY_ERROR.lock().ok().and_then(|guard| guard.clone())
}

/// Frontend-facing: the Vision Pro discovery-startup error, if discovery couldn't start
/// (mDNS blocked / Local Network permission denied). `None` = discovery is running.
/// The device list's empty state uses this to explain *why* nothing showed up.
#[tauri::command]
pub fn vision_discovery_error() -> Option<String> {
    discovery_error()
}

fn browser() -> &'static VisionBrowser {
    BROWSER.get_or_init(new_browser)
}

/// macOS: browse through the system Bonjour daemon — see the `bonjour` module for why
/// raw multicast is a trap here.
#[cfg(target_os = "macos")]
fn new_browser() -> VisionBrowser {
    let devices: Arc<Mutex<HashMap<String, Discovered>>> = Arc::new(Mutex::new(HashMap::new()));
    let by_fullname: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    set_discovery_error(None);
    bonjour::start(devices.clone(), by_fullname);
    VisionBrowser { devices }
}

/// Non-macOS: mdns_sd's own multicast sockets (no local-network gatekeeper there).
#[cfg(not(target_os = "macos"))]
fn new_browser() -> VisionBrowser {
    {
        let daemon = match ServiceDaemon::new() {
            Ok(daemon) => {
                set_discovery_error(None);
                Some(daemon)
            }
            Err(e) => {
                tracing::error!(
                    target: "vision",
                    "Couldn't start network discovery (mDNS): {e}. Vision Pro discovery is \
                     disabled. On macOS this usually means Local Network permission was denied \
                     (System Settings ▸ Privacy & Security ▸ Local Network) or a VPN/firewall is \
                     blocking multicast."
                );
                set_discovery_error(Some(format!("Couldn't start network discovery: {e}")));
                None
            }
        };
        let devices: Arc<Mutex<HashMap<String, Discovered>>> = Arc::new(Mutex::new(HashMap::new()));
        let by_fullname: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

        if let Some(daemon) = &daemon {
            for service in [MANUAL_PAIRING_SERVICE, REMOTE_PAIRING_SERVICE] {
                let recv = match daemon.browse(service) {
                    Ok(recv) => recv,
                    Err(e) => {
                        tracing::error!(
                            target: "vision",
                            "Couldn't browse mDNS service {service}: {e}"
                        );
                        set_discovery_error(Some(format!("Couldn't start network discovery: {e}")));
                        continue;
                    }
                };
                {
                    let is_manual = service == MANUAL_PAIRING_SERVICE;
                    let devices_bg = devices.clone();
                    let by_fullname_bg = by_fullname.clone();
                    // Blocking receive on a dedicated thread per service: no async
                    // runtime needed, so this works from setup or from a command.
                    std::thread::spawn(move || {
                        while let Ok(event) = recv.recv() {
                            match event {
                                ServiceEvent::ServiceResolved(info) => {
                                    if let Some((key, dev)) = parse_resolved(&info, is_manual) {
                                        by_fullname_bg
                                            .lock()
                                            .unwrap()
                                            .insert(info.get_fullname().to_string(), key.clone());
                                        merge_device(&devices_bg, key, dev);
                                    }
                                }
                                ServiceEvent::ServiceRemoved(_, fullname) => {
                                    remove_fullname(&devices_bg, &by_fullname_bg, &fullname);
                                }
                                _ => {}
                            }
                        }
                    });
                }
            }
        }

        // `by_fullname` isn't stored on the struct: the browse threads (which run for
        // the process lifetime) hold their own `Arc` clones, so the shared map stays
        // alive without the struct holding a reference it never reads.
        VisionBrowser {
            devices,
            _daemon: daemon,
        }
    }
}

/// Turn a resolved mDNS record into `(device key, Discovered)`, or `None` if it isn't a
/// Vision Pro we can use.
#[cfg(not(target_os = "macos"))]
fn parse_resolved(info: &ResolvedService, is_manual: bool) -> Option<(String, Discovered)> {
    let addr = info.get_addresses_v4().into_iter().next()?;
    if is_manual {
        // Manual-pairing: friendly name from the fullname, dynamic pairing port.
        let name = instance_name(info.get_fullname());
        Some((
            canonical(&name),
            Discovered {
                name,
                ip: addr.to_string(),
                ips: vec![addr.to_string()],
                manual_pairing_port: Some(info.get_port()),
            },
        ))
    } else {
        // RemotePairing: iPhones advertise it too, so filter to Vision Pro hostnames.
        let hostname = info.get_hostname();
        if !is_vision_hostname(hostname) {
            return None;
        }
        let name = hostname_label(hostname);
        Some((
            canonical(&name),
            Discovered {
                name,
                ip: addr.to_string(),
                ips: vec![addr.to_string()],
                manual_pairing_port: None,
            },
        ))
    }
}

/// Insert/refresh a device. The remotepairing view (no manual port) must not clobber
/// what the manual-pairing view knows better: the pairable port, and the real
/// friendly name — the remotepairing record only carries a hostname-derived label
/// ("Sams AppleVisionPro" for a headset named "Sam's Apple Vision Pro"), so without
/// this the displayed name flip-flops with whichever record resolved last.
///
/// Addresses are UNIONED, not replaced: each view (and each re-announcement) resolves
/// a partial, point-in-time set, and whichever resolves last would otherwise wipe out
/// the other's — including the one routable address, when a later resolve caught only
/// the 169.254.x link-local. Fresh addresses are kept ahead of remembered ones within
/// the routable/link-local sort, so after a DHCP move the new address is tried first
/// and stale ones drift to the tail (and off the end of the cap).
fn merge_device(devices: &Arc<Mutex<HashMap<String, Discovered>>>, key: String, mut dev: Discovered) {
    let mut guard = devices.lock().unwrap();
    if let Some(existing) = guard.get(&key) {
        if dev.manual_pairing_port.is_none() {
            dev.manual_pairing_port = existing.manual_pairing_port;
            dev.name = existing.name.clone();
        }
        for ip in &existing.ips {
            if !dev.ips.contains(ip) {
                dev.ips.push(ip.clone());
            }
        }
        dev.ips.sort_by_key(|i| i.starts_with("169.254."));
        dev.ips.truncate(8);
        if let Some(first) = dev.ips.first() {
            dev.ip = first.clone();
        }
    }
    guard.insert(key, dev);
}

/// A service instance went away: drop its record, and drop the device itself only if
/// no other record still references it (a VP is often visible via two services, or
/// the same service on several interfaces).
fn remove_fullname(
    devices: &Arc<Mutex<HashMap<String, Discovered>>>,
    by_fullname: &Arc<Mutex<HashMap<String, String>>>,
    fullname: &str,
) {
    let key = by_fullname.lock().unwrap().remove(fullname);
    if let Some(key) = key {
        let still_present = by_fullname.lock().unwrap().values().any(|k| *k == key);
        if !still_present {
            devices.lock().unwrap().remove(&key);
        } else if fullname.contains("_remotepairing-manual-pairing._tcp") {
            // The headset withdrew its manual-pairing service but is still visible via
            // remotepairing, so the device stays — but its pairing port MUST NOT. A
            // headset re-advertises constantly with a NEW port each time (a field log
            // showed 64421→64429 within 20 minutes), so a retained port is a port
            // nothing is listening on: connecting to it fails, often as "no route to
            // host" when the headset is also dozing. Better to report it as not
            // currently pairable and wait for the next announcement.
            if let Some(dev) = devices.lock().unwrap().get_mut(&key) {
                dev.manual_pairing_port = None;
            }
        }
    }
}

/// System-Bonjour (dns_sd → mDNSResponder) discovery backend, macOS only.
///
/// On macOS 15+ the Local Network privacy layer decides per-app whether local traffic
/// is allowed — and for an app doing its own multicast (mdns_sd) the OS frequently
/// fails to attribute the traffic to the app. When that happens there is no permission
/// prompt, the app never appears in System Settings ▸ Privacy & Security ▸ Local
/// Network, and the packets are silently dropped: user logs show mdns_sd joining its
/// multicast groups cleanly and then receiving nothing, ever. (Dev runs never hit this
/// because terminal-spawned processes get automatic local-network access — TN3179.)
///
/// Browsing through mDNSResponder instead is Apple's designed path: the daemon does
/// the multicast, the browse is attributed to us, macOS reliably shows the prompt and
/// lists iloader in the Settings pane, and interface churn / sleep-wake are the
/// daemon's problem, not ours.
#[cfg(target_os = "macos")]
mod bonjour {
    use super::*;
    use async_dnssd::{BrowseResult, BrowsedFlags, ScopedSocketAddr};
    use futures::StreamExt;

    /// Budget for turning one browse announcement into a connectable (host, port,
    /// IPv4). Generous — a re-announcement retriggers resolution anyway.
    const RESOLVE_BUDGET: Duration = Duration::from_secs(10);

    /// Once a routable address is in hand, how long to keep waiting for siblings.
    /// The address stream never ends on its own, so without a cutoff a headset
    /// advertising a single address would stall until the full budget expired.
    const ADDR_COLLECT_GRACE: Duration = Duration::from_millis(400);

    /// How long to keep waiting when everything so far is link-local (169.254.x).
    /// Those are usually unreachable from the Mac's Wi-Fi, and the resolver often
    /// reports them first, so it's worth waiting a bit longer for a routable one.
    const ADDR_LINK_LOCAL_WAIT: Duration = Duration::from_millis(2500);

    pub(super) fn start(
        devices: Arc<Mutex<HashMap<String, Discovered>>>,
        by_fullname: Arc<Mutex<HashMap<String, String>>>,
    ) {
        for service in [MANUAL_PAIRING_SERVICE, REMOTE_PAIRING_SERVICE] {
            // dns_sd takes the bare reg type; the ".local." domain is implied.
            let reg_type = service.strip_suffix(".local.").unwrap_or(service);
            let is_manual = service == MANUAL_PAIRING_SERVICE;
            let devices = devices.clone();
            let by_fullname = by_fullname.clone();
            tauri::async_runtime::spawn(async move {
                browse_loop(reg_type, is_manual, devices, by_fullname).await;
            });
        }
    }

    /// Browse one service type forever. The stream only terminates on error (e.g.
    /// mDNSResponder restarted, or the browse was rejected), so on termination we
    /// surface the error and retry on a slow cadence — cheap, and it recovers a
    /// just-granted Local Network permission without relaunching.
    async fn browse_loop(
        reg_type: &'static str,
        is_manual: bool,
        devices: Arc<Mutex<HashMap<String, Discovered>>>,
        by_fullname: Arc<Mutex<HashMap<String, String>>>,
    ) {
        loop {
            let mut browse = async_dnssd::browse(reg_type);
            // A rejected browse (PolicyDenied etc.) errors within moments; one that
            // survives its first seconds was accepted, so a stale startup error (e.g.
            // permission was off, user just switched it on) no longer applies —
            // clear it even before any device announces itself.
            let mut accepted = false;
            loop {
                let event = if accepted {
                    browse.next().await
                } else {
                    tokio::select! {
                        event = browse.next() => event,
                        _ = tokio::time::sleep(Duration::from_secs(3)) => {
                            accepted = true;
                            set_discovery_error(None);
                            continue;
                        }
                    }
                };
                let Some(event) = event else { break };
                let event = match event {
                    Ok(event) => event,
                    Err(e) => {
                        tracing::error!(
                            target: "vision",
                            "Bonjour browse for {reg_type} failed: {e}"
                        );
                        set_discovery_error(Some(explain_browse_error(&e)));
                        break;
                    }
                };
                // Events are flowing, so discovery is demonstrably working.
                set_discovery_error(None);
                let fullname =
                    format!("{}.{}{}", event.service_name, event.reg_type, event.domain);
                if event.flags.contains(BrowsedFlags::ADD) {
                    let devices = devices.clone();
                    let by_fullname = by_fullname.clone();
                    // Resolve in its own task so a slow SRV/address lookup doesn't
                    // stall the browse stream (and other devices' events).
                    tauri::async_runtime::spawn(async move {
                        resolve_and_track(&event, is_manual, fullname, devices, by_fullname).await;
                    });
                } else {
                    tracing::debug!(target: "vision", "Bonjour: {fullname} went away");
                    remove_fullname(&devices, &by_fullname, &fullname);
                }
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }

    /// Resolve one browse Add and keep the device's address list current — the dns_sd
    /// analogue of `parse_resolved`, plus late-address tracking.
    ///
    /// Collect EVERY advertised IPv4, not just the first. A headset commonly
    /// advertises several — e.g. its Wi-Fi address plus a 169.254.x link-local from
    /// another link — and the first one out of the resolver is often not the one
    /// this Mac can route to, which surfaces to the user as "No route to host".
    ///
    /// The address stream stays open indefinitely (it reports future changes too),
    /// so this must never be wrapped in a single timeout around the whole loop:
    /// doing that threw away every address collected so far when the budget
    /// expired, which silently dropped any headset advertising just one address.
    ///
    /// The device is published once the list has briefly settled, but the stream is
    /// then drained for the REST of the budget, folding late arrivals into the map:
    /// the resolver has been seen reporting only the link-local within any reasonable
    /// settle window, and a device stuck on 169.254.x until the next re-announcement
    /// (30-60s away) is exactly the "No route to host" failure again.
    async fn resolve_and_track(
        event: &BrowseResult,
        is_manual: bool,
        fullname: String,
        devices: Arc<Mutex<HashMap<String, Discovered>>>,
        by_fullname: Arc<Mutex<HashMap<String, String>>>,
    ) {
        let Ok(Some(Ok(resolved))) =
            tokio::time::timeout(RESOLVE_BUDGET, event.resolve().next()).await
        else {
            tracing::debug!(
                target: "vision",
                "Bonjour: couldn't resolve {} (device gone or asleep?)",
                event.service_name
            );
            return;
        };
        // remotepairing is advertised by iPhones too (they're handled over usbmux);
        // only the hostname says which kind of device this is.
        if !is_manual && !is_vision_hostname(&resolved.host_target) {
            tracing::debug!(
                target: "vision",
                "Bonjour: {} is {} — not a Vision Pro, ignoring",
                event.service_name,
                resolved.host_target
            );
            return;
        }
        let name = if is_manual {
            event.service_name.replace('\u{a0}', " ")
        } else {
            hostname_label(&resolved.host_target)
        };
        let key = canonical(&name);
        let port = resolved.port;

        let publish = |ips: &[String], first_publish: bool| {
            let mut ips = ips.to_vec();
            // Try routable addresses before link-local ones.
            ips.sort_by_key(|i| i.starts_with("169.254."));
            let Some(ip) = ips.first().cloned() else { return };
            if first_publish {
                tracing::info!(
                    target: "vision",
                    "Bonjour: {} at {} (manual pairing port: {:?})",
                    name,
                    ip,
                    is_manual.then_some(port)
                );
            }
            by_fullname
                .lock()
                .unwrap()
                .insert(fullname.clone(), key.clone());
            merge_device(
                &devices,
                key.clone(),
                Discovered {
                    name: name.clone(),
                    ip,
                    ips,
                    manual_pairing_port: is_manual.then_some(port),
                },
            );
        };

        let mut addrs = resolved.resolve_socket_address();
        let mut ips: Vec<String> = Vec::new();
        let mut published = false;
        let hard_deadline = tokio::time::Instant::now() + RESOLVE_BUDGET;
        // First publish waits for the list to settle briefly once something routable
        // is in hand, longer while everything so far is link-local.
        let mut publish_at: Option<tokio::time::Instant> = None;
        loop {
            let now = tokio::time::Instant::now();
            if now >= hard_deadline {
                break;
            }
            if !published && publish_at.is_some_and(|p| now >= p) {
                publish(&ips, true);
                published = true;
            }
            let until = if published {
                hard_deadline
            } else {
                publish_at.map_or(hard_deadline, |p| p.min(hard_deadline))
            };
            match tokio::time::timeout(until - now, addrs.next()).await {
                Ok(Some(Ok(addr))) => {
                    if let ScopedSocketAddr::V4 { address, .. } = addr.address {
                        let s = address.to_string();
                        if ips.contains(&s) {
                            continue;
                        }
                        ips.push(s.clone());
                        if published {
                            // A Remove may have raced us; folding an address in then
                            // would resurrect the withdrawn record — and its dead
                            // manual-pairing port. Only update while it's still live.
                            if by_fullname.lock().unwrap().contains_key(&fullname) {
                                tracing::info!(
                                    target: "vision",
                                    "Bonjour: {name} gained address {s}"
                                );
                                publish(&ips, false);
                            }
                        } else {
                            let have_routable = ips.iter().any(|i| !i.starts_with("169.254."));
                            let wait = if have_routable {
                                ADDR_COLLECT_GRACE
                            } else {
                                ADDR_LINK_LOCAL_WAIT
                            };
                            publish_at = Some(tokio::time::Instant::now() + wait);
                        }
                    }
                }
                Ok(Some(Err(_))) => continue,
                // Stream ended: use what we have.
                Ok(None) => break,
                // A deadline passed; the loop top decides whether it was the publish
                // point or the end of the budget.
                Err(_) => continue,
            }
        }
        if !published {
            publish(&ips, true);
        }
    }

    /// A denied Local Network permission surfaces as dns_sd error -65570
    /// (kDNSServiceErr_PolicyDenied; async-dnssd's older tables call it
    /// "ConnectionPending"). Translate that to something a user can act on.
    fn explain_browse_error(e: &std::io::Error) -> String {
        let raw = e.to_string();
        let lower = raw.to_lowercase();
        if raw.contains("65570") || lower.contains("policy") || lower.contains("connection pending")
        {
            "Local Network permission is off for iloader — enable it in System Settings ▸ \
             Privacy & Security ▸ Local Network, then try again."
                .to_string()
        } else {
            format!("Couldn't start network discovery: {raw}")
        }
    }
}

/// Start the persistent Vision Pro browser at app launch so the multicast group is
/// warm well before the first device-list refresh. Idempotent.
pub fn start_discovery() {
    let _ = browser();
}

/// Poll the persistent browser's warm set until it's non-empty or the (short) budget
/// runs out. Warm calls return immediately; only a cold first call after launch waits.
async fn snapshot_soon() -> Vec<Discovered> {
    let b = browser();
    let mut snap = b.snapshot();
    for _ in 0..16 {
        if !snap.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        snap = b.snapshot();
    }
    snap
}

/// The addresses to dial for the named headset: everything discovery currently knows
/// (routable first), with `fallback_ip` — whatever the caller captured earlier —
/// appended as a last resort. Connect-time callers must use this rather than a cached
/// single address: a headset advertises several addresses and only some are routable,
/// its address moves with DHCP, and the cached one is often the 169.254.x link-local.
pub fn live_ips(name: &str, fallback_ip: &str) -> Vec<String> {
    let mut ips = browser().get(name).map(|d| d.ips).unwrap_or_default();
    if !ips.iter().any(|i| i == fallback_ip) {
        ips.push(fallback_ip.to_string());
    }
    ips
}

/// A live tunnel to a paired Vision Pro, over which RSD services can be opened.
pub struct VisionSession {
    pub adapter: AdapterHandle,
    pub handshake: RsdHandshake,
}

impl VisionSession {
    /// Establish the tunnel using the RP pairing file bytes, trying every address the
    /// headset advertises.
    pub async fn connect_any(ips: &[String], pairing: &[u8]) -> Result<Self, AppError> {
        let mut pf = RpPairingFile::from_bytes(pairing)
            .map_err(|e| AppError::RemotePairing(format!("Invalid pairing file: {e:?}")))?;

        // 1. pair-verify on the RSD to derive the tunnel key.
        let (s1, ip) = connect_any(ips, RSD_PORT, "Couldn't reach the Vision Pro (RSD)").await?;
        let ip = ip.as_str();
        let mut client = RemotePairingClient::new(RpPairingSocket::new(s1), "iloader");
        client
            .connect(&mut pf, || async { "000000".to_string() })
            .await
            .map_err(|e| match e {
                // The socket died mid-exchange — a dozing headset, not a verdict on
                // the pairing.
                idevice::IdeviceError::Socket(io) => {
                    connect_err("Lost the Vision Pro during pair-verify", ip, RSD_PORT, io)
                }
                e => AppError::VisionPairingRejected(format!(
                    "pair-verify failed (is the Vision Pro still paired?): {e:?}"
                )),
            })?;
        let key = client.encryption_key().to_vec();

        // 2. ask the device to open a tunnel listener, connect + TLS-PSK.
        let tport = client
            .create_tcp_listener()
            .await
            .map_err(|e| ap_err("create tunnel listener", e))?;
        let started = std::time::Instant::now();
        let s2 = TcpStream::connect((ip, tport)).await.map_err(|e| {
            AppError::VisionUnreachable(connect_err_message(
                "Couldn't open the tunnel to the Vision Pro",
                ip,
                tport,
                e,
                Some(started.elapsed()),
            ))
        })?;
        tracing::info!(
            target: "vision",
            "tunnel: TCP connected to {ip}:{tport} in {:?}",
            started.elapsed()
        );
        let tunnel = connect_tls_psk_tunnel_native(s2, &key)
            .await
            .map_err(|e| ap_err("TLS-PSK tunnel", e))?;
        let info = tunnel.info.clone();

        // 3. software TCP stack over the tunnel.
        let our_ip = info
            .client_address
            .parse()
            .map_err(|e| ap_err("client ip", e))?;
        let their_ip = info
            .server_address
            .parse()
            .map_err(|e| ap_err("server ip", e))?;
        let mut adapter = Adapter::new(Box::new(tunnel.into_inner()), our_ip, their_ip);
        adapter.set_mss((info.mtu as usize).saturating_sub(60));
        let mut adapter = adapter.to_async_handle();

        // 4. RSD handshake -> service map.
        let rsd_stream = adapter
            .connect(info.server_rsd_port)
            .await
            .map_err(|e| ap_err("connect RSD over tunnel", e))?;
        let handshake = RsdHandshake::new(rsd_stream)
            .await
            .map_err(|e| ap_err("RSD handshake", e))?;

        Ok(Self { adapter, handshake })
    }

    /// Open an RSD service (e.g. `InstallationProxyClient`, `AfcClient`) over the tunnel.
    pub async fn service<S: RsdService>(&mut self) -> Result<S, AppError> {
        S::connect_rsd(&mut self.adapter, &mut self.handshake)
            .await
            .map_err(|e| ap_err(&format!("connect RSD service {}", S::rsd_service_name()), e))
    }

    /// The device UDID, read from the RSD handshake properties.
    pub fn udid(&self) -> Result<String, AppError> {
        self.handshake
            .properties
            .get("UniqueDeviceID")
            .and_then(|v| v.as_string())
            .map(str::to_string)
            .ok_or_else(|| {
                AppError::RemotePairing("could not read the device UDID over RSD".into())
            })
    }
}

/// Open a short-lived tunnel just to read the device UDID (needed to register the VP
/// with the developer account before signing). Kept separate so it can be dropped
/// before the network-bound signing that follows.
pub async fn read_udid_any(ips: &[String], pairing: &[u8]) -> Result<String, AppError> {
    VisionSession::connect_any(ips, pairing).await?.udid()
}

/// Upload a signed `.app` bundle to PublicStaging over AFC, then install it via
/// installation_proxy — all over the RP tunnel. `progress` receives 0..=100.
pub async fn install_app(
    session: &mut VisionSession,
    app_path: &Path,
    progress: impl Fn(u64),
) -> Result<(), AppError> {
    let name = app_path
        .file_name()
        .ok_or_else(|| AppError::RemotePairing("app path has no final component".into()))?
        .to_string_lossy()
        .to_string();
    let dir = format!("PublicStaging/{name}");

    // Walk the bundle up front so the upload is a flat list — a mid-bundle AFC
    // failure can then retry a single file on a fresh AFC connection without
    // re-walking (or re-uploading) anything already transferred.
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    collect_upload_entries(app_path, &dir, &mut dirs, &mut files)?;

    let mut afc = session.service::<AfcClient>().await?;
    for d in &dirs {
        afc.mk_dir(d)
            .await
            .map_err(|e| ap_err(&format!("AFC mkdir {d}"), e))?;
    }

    // Multi-MB uploads over the userspace TCP tunnel occasionally drop the AFC
    // stream mid-file (field report: `AFC write …: Socket(NotConnected)` installing a
    // large app). The tunnel itself usually survives its streams, so retry the file
    // on a freshly opened AFC connection before giving up; if the tunnel really died,
    // reopening the service fails and that error is surfaced instead.
    for (local, remote) in &files {
        let mut attempts = 0;
        loop {
            match afc_upload_file(&mut afc, local, remote).await {
                Ok(()) => break,
                Err(e) if attempts < 2 => {
                    attempts += 1;
                    tracing::warn!(
                        target: "vision",
                        "AFC upload of {remote} failed ({e}); reconnecting AFC and retrying \
                         ({attempts}/2)"
                    );
                    tokio::time::sleep(Duration::from_millis(750)).await;
                    afc = session.service::<AfcClient>().await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    let mut inst = session.service::<InstallationProxyClient>().await?;

    let mut options = plist::Dictionary::new();
    options.insert("PackageType".into(), plist::Value::String("Developer".into()));

    inst.install_with_callback(
        dir,
        Some(plist::Value::Dictionary(options)),
        async |(percentage, _)| {
            progress(percentage);
        },
        (),
    )
    .await
    .map_err(|e| ap_err("installation_proxy install failed", e))?;

    Ok(())
}

/// Walk `path` recursively, listing every directory (parents before children, so they
/// can be mk_dir'd in order) and every file with its destination AFC path.
fn collect_upload_entries(
    path: &Path,
    afc_path: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<(PathBuf, String)>,
) -> Result<(), AppError> {
    dirs.push(afc_path.to_string());
    for entry in std::fs::read_dir(path)
        .map_err(|e| AppError::Filesystem(format!("read_dir {path:?}"), e.to_string()))?
    {
        let entry =
            entry.map_err(|e| AppError::Filesystem("read dir entry".into(), e.to_string()))?;
        let p = entry.path();
        let child_name = p
            .file_name()
            .ok_or_else(|| AppError::Filesystem("dir entry has no name".into(), String::new()))?
            .to_string_lossy()
            .to_string();
        let child_afc = format!("{afc_path}/{child_name}");
        if p.is_dir() {
            collect_upload_entries(&p, &child_afc, dirs, files)?;
        } else {
            files.push((p, child_afc));
        }
    }
    Ok(())
}

/// Upload one file to `afc_path`. WrOnly truncates, so a retry after a partial write
/// starts the file over cleanly.
async fn afc_upload_file(
    afc: &mut AfcClient,
    path: &Path,
    afc_path: &str,
) -> Result<(), AppError> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::Filesystem(format!("read {path:?}"), e.to_string()))?;
    let mut fh = afc
        .open(afc_path.to_string(), AfcFopenMode::WrOnly)
        .await
        .map_err(|e| ap_err(&format!("AFC open {afc_path}"), e))?;
    fh.write_entire(&bytes)
        .await
        .map_err(|e| ap_err(&format!("AFC write {afc_path}"), e))?;
    fh.close().await.map_err(|e| ap_err("AFC close", e))?;
    Ok(())
}

/// Find an installed app whose bundle id contains `needle` (case-insensitive).
pub async fn find_app(session: &mut VisionSession, needle: &str) -> Result<String, AppError> {
    let mut inst = session.service::<InstallationProxyClient>().await?;
    let apps = inst
        .get_apps(None, None)
        .await
        .map_err(|e| ap_err("get_apps", e))?;
    apps.keys()
        .find(|b| b.to_lowercase().contains(&needle.to_lowercase()))
        .cloned()
        .ok_or_else(|| AppError::RemotePairing(format!("no installed app matching '{needle}'")))
}

/// Write pairing bytes into `bundle`'s Documents as `path` (over the tunnel).
pub async fn place_into(
    session: &mut VisionSession,
    bundle: &str,
    path: &str,
    pairing: &[u8],
) -> Result<(), AppError> {
    let ha = session.service::<HouseArrestClient>().await?;
    let mut afc = ha
        .vend_documents(bundle.to_string())
        .await
        .map_err(|e| ap_err(&format!("vend documents for {bundle}"), e))?;

    // vend_documents roots AFC at the app CONTAINER, but only /Documents is
    // accessible (HouseArrest semantics), so write there. Create any intermediate
    // directories one level at a time — AFC mk_dir isn't recursive (LiveContainer's
    // pairing path, SideStore/Documents/…, is nested).
    if let Some((dir, _)) = path.rsplit_once('/') {
        let mut cur = String::from("/Documents");
        for comp in dir.split('/').filter(|c| !c.is_empty()) {
            cur.push('/');
            cur.push_str(comp);
            let _ = afc.mk_dir(cur.clone()).await;
        }
    }
    let mut fh = afc
        .open(format!("/Documents/{path}"), AfcFopenMode::WrOnly)
        .await
        .map_err(|e| ap_err(&format!("open /Documents/{path}"), e))?;
    fh.write_entire(pairing)
        .await
        .map_err(|e| ap_err("write pairing", e))?;
    fh.close().await.map_err(|e| ap_err("close", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Device-list integration + first-time pairing (the Tauri-facing surface).
// ---------------------------------------------------------------------------

/// Discover Vision Pro(s) over mDNS and present them as `DeviceInfo` for the unified
/// device list. `paired` reflects whether a reusable RP pairing file is already
/// stored, so the frontend can select the device directly instead of prompting for
/// the headset code. Never errors — a discovery failure just yields an empty list so
/// it can't hide usbmux devices.
pub async fn list_vision_devices(app: &AppHandle) -> Vec<DeviceInfo> {
    let discovered = snapshot_soon().await;
    discovered
        .into_iter()
        .map(|d| {
            let paired = retrieve_pairing_data(app, &pairing_storage_key(&d.name))
                .ok()
                .flatten()
                .is_some();
            DeviceInfo {
                id: d.synthetic_id(),
                name: d.name,
                udid: String::new(),
                connection_type: "Wireless".to_string(),
                version: "visionOS".to_string(),
                device_class: Some("RealityDevice".to_string()),
                product_type: None,
                transport: DeviceTransport::Vision,
                ip: Some(d.ip),
                paired,
            }
        })
        .collect()
}

/// Select an already-paired Vision Pro: load its stored pairing file and verify it by
/// opening a tunnel (which also yields the device UDID). Returns `(pairing, udid)`.
pub async fn select_vision_device(
    app: &AppHandle,
    device: &DeviceInfo,
) -> Result<(Vec<u8>, String), AppError> {
    let ip = device
        .ip
        .clone()
        .ok_or_else(|| AppError::RemotePairing("Vision Pro has no IP address".into()))?;
    let key = pairing_storage_key(&device.name);
    let pairing = retrieve_pairing_data(app, &key)?.ok_or_else(|| {
        AppError::RemotePairing(
            "This Vision Pro isn't paired yet — enter the code shown on the headset to pair.".into(),
        )
    })?;
    // Prefer the freshly-advertised addresses over whatever the frontend passed —
    // a headset's address changes with DHCP, and the list may hold several.
    let ips = live_ips(&device.name, &ip);

    // Verify the stored pairing by opening a tunnel.
    match read_udid_any(&ips, &pairing).await {
        Ok(udid) => Ok((pairing, udid)),
        // The headset answered and explicitly rejected the stored pairing during
        // pair-verify (e.g. this Mac was removed from its Remote Devices) — the one
        // case that proves the pairing is stale. Drop it so the frontend re-prompts
        // for the code.
        Err(AppError::VisionPairingRejected(e)) => {
            let _ = delete_pairing_data(app, &key);
            Err(AppError::RemotePairing(format!(
                "This Vision Pro's saved pairing is no longer valid — pair again with the code on \
                 the headset. ({e})"
            )))
        }
        // Anything else — unreachable, a tunnel-setup hiccup, an RSD failure — says
        // nothing about the pairing's validity. KEEP it: discarding a good pairing
        // here used to force users into a needless re-pair (which then fails too, if
        // the headset is simply asleep).
        Err(e) => Err(e),
    }
}

/// First-time wireless pairing with a Vision Pro. Connects to its manual-pairing
/// service; the headset shows a 6-digit code which the user types into iloader (sent
/// back via the `vision-pair-code` event). On success the reusable RP pairing file is
/// stored and the device becomes the selected device.
///
/// Emits `vision-pair-status`: "connecting" | "awaiting-code" | "verifying" | "paired".
#[tauri::command]
pub async fn vision_pair(
    app: AppHandle,
    window: Window,
    device_state: State<'_, DeviceInfoMutex>,
    cancel_state: State<'_, PairingCancelToken>,
    device: DeviceInfo,
) -> Result<(), AppError> {
    let token = CancellationToken::new();
    {
        let mut guard = cancel_state.lock().unwrap();
        if let Some(old) = guard.replace(token.clone()) {
            old.cancel();
        }
    }

    let result = vision_pair_inner(&app, &window, &device, token.clone()).await;

    {
        // Single-flight modal flow: clear our cancel token when we're done.
        let mut guard = cancel_state.lock().unwrap();
        *guard = None;
    }

    let (pairing, udid) = result?;

    let mut info = device;
    info.udid = udid;
    info.paired = true;
    {
        let mut ds = device_state.lock().unwrap();
        *ds = Some(DeviceInfoWithPairing { info, pairing });
    }
    let _ = window.emit("vision-pair-status", "paired");
    Ok(())
}

async fn vision_pair_inner(
    app: &AppHandle,
    window: &Window,
    device: &DeviceInfo,
    cancel: CancellationToken,
) -> Result<(Vec<u8>, String), AppError> {
    let _ = window.emit("vision-pair-status", "connecting");

    // Resolve the CURRENT (dynamic) manual-pairing port from the persistent browser,
    // matching this VP by name. The port is re-announced as the device advertises, so
    // the warm set tracks it; we wait briefly only if nothing's cached yet.
    // Wait for a CURRENTLY-advertised manual-pairing port, not merely for the device
    // to be known. The headset withdraws and re-announces this service every ~30-60s
    // with a fresh port, so it is routinely absent for a few seconds at a time; only
    // a port from a live announcement is connectable.
    let b = browser();
    let mut dev = None;
    let mut seen_without_port = false;
    for _ in 0..PAIRABLE_WAIT_POLLS {
        if let Some(d) = b.get(&device.name) {
            if d.manual_pairing_port.is_some() {
                dev = Some(d);
                break;
            }
            seen_without_port = true;
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Canceled("Wireless pairing".into())),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }

    let Some(dev) = dev else {
        // Distinguish "we can't see it at all" from "we see it but it never offers
        // pairing" — an existing host pairing (iloader's, Xcode's, another Mac's)
        // suppresses the manual-pairing service entirely.
        return Err(AppError::RemotePairing(if seen_without_port {
            "This Vision Pro already holds a host pairing (possibly Xcode's), so it isn't \
             accepting new pairing requests. To pair iloader: on the headset, open Settings → \
             General → Remote Devices, remove the existing entry, then try again. An Xcode \
             pairing removed this way can simply be re-paired afterwards — iloader's and \
             Xcode's pairings coexist once both are set up. Tip: if this Mac is already \
             paired via Xcode, the headset may also appear in the device list as a \
             \"Network\" device, which you can sideload to directly without pairing iloader."
                .into()
        } else {
            "Couldn't find the Vision Pro's pairing service over Wi-Fi. Make sure it's on the \
             same network, awake (put the headset on), and that Developer Mode is on."
                .into()
        }));
    };
    let port = dev
        .manual_pairing_port
        .expect("loop only exits with a device that has a port");

    // Pair, with one silent retry for failures that happen BEFORE the user was asked
    // for a code. If the headset's code screen isn't up when the session starts, the
    // first session can be a dud (and was guaranteed to be, before the vendored
    // idevice fix for `awaitingUserConsent`); the user can't have mistyped anything
    // yet, so retrying transparently beats surfacing an error.
    let mut attempt = 0;
    let pf = loop {
        // Re-resolve address/port from the live browser each attempt — the
        // manual-pairing port is dynamic and can rotate when the session cycles.
        let (ips, port) = match b.get(&device.name) {
            Some(d) if d.manual_pairing_port.is_some() => {
                (d.ips.clone(), d.manual_pairing_port.unwrap())
            }
            _ => (dev.ips.clone(), port),
        };

        let code_requested = Arc::new(AtomicBool::new(false));

        // Walk every advertised address — only some are routable from this Mac.
        let (stream, _reached_ip) = tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Canceled("Wireless pairing".into())),
            res = connect_any(&ips, port, "Couldn't reach the Vision Pro to pair") => res?,
        };

        let mut client = RemotePairingClient::new(RpPairingSocket::new(stream), "iloader");
        let mut pf = RpPairingFile::generate("iloader");

        // The device shows the code; we prompt the user for it via the frontend.
        // idevice calls this once, after the code is on the headset.
        let win = window.clone();
        let requested = code_requested.clone();
        let code_cb = move || {
            let win = win.clone();
            requested.store(true, Ordering::SeqCst);
            async move {
                let (tx, rx) = tokio::sync::oneshot::channel::<String>();
                let tx = std::sync::Mutex::new(Some(tx));
                let handler = win.listen("vision-pair-code", move |event| {
                    if let Ok(mut guard) = tx.lock()
                        && let Some(sender) = guard.take()
                    {
                        let raw = event.payload();
                        let code =
                            serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_string());
                        let _ = sender.send(code);
                    }
                });
                let _ = win.emit("vision-pair-status", "awaiting-code");
                let code = rx.await.unwrap_or_default();
                win.unlisten(handler);
                code.chars().filter(|c| c.is_ascii_digit()).take(6).collect::<String>()
            }
        };

        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Canceled("Wireless pairing".into())),
            res = client.connect(&mut pf, code_cb) => res,
        };

        match result {
            Ok(()) => break pf,
            Err(e) => {
                if attempt == 0 && !code_requested.load(Ordering::SeqCst) {
                    tracing::warn!(
                        target: "vision",
                        "first pairing attempt failed before any code prompt ({e:?}); retrying"
                    );
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(750)).await;
                    continue;
                }
                return Err(AppError::RemotePairing(format!(
                    "Pairing failed: {e:?}\n(If it mentions 'missing server proof', the code was \
                     mistyped — try pairing again.)"
                )));
            }
        }
    };

    let bytes = pf.to_bytes();
    store_pairing_data(app, &pairing_storage_key(&device.name), &bytes)?;

    // Verify the saved pairing by opening a tunnel, which also gives us the UDID. A
    // failure here doesn't discard the (successful) pairing — sideload re-reads the
    // UDID over a fresh tunnel and will surface any real problem then.
    let _ = window.emit("vision-pair-status", "verifying");
    let udid = read_udid_any(&dev.ips, &bytes).await.unwrap_or_default();

    Ok((bytes, udid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_hostname_filter_accepts_all_headset_name_shapes() {
        // Default device name hyphenates: this exact shape was field-reported as
        // wrongly filtered out ("not a Vision Pro, ignoring").
        assert!(is_vision_hostname("Apple-Vision-Pro.local."));
        assert!(is_vision_hostname("Apple-Vision-Pro-2.local."));
        // Custom-named headsets concatenate.
        assert!(is_vision_hostname("Sams-AppleVisionPro.local."));
        assert!(is_vision_hostname("RealityDevice.local."));
        // iPhones/AppleTVs advertise _remotepairing._tcp too and must stay hidden.
        assert!(!is_vision_hostname("Sams-iPhone.local."));
        assert!(!is_vision_hostname("Living-Room-Apple-TV.local."));
    }

    #[test]
    fn canonical_unifies_friendly_name_and_hostname() {
        assert_eq!(
            canonical("Sam’s Apple Vision Pro"),
            canonical("Sams-AppleVisionPro")
        );
    }

    fn disc(name: &str, ips: &[&str], port: Option<u16>) -> Discovered {
        Discovered {
            name: name.into(),
            ip: ips.first().unwrap().to_string(),
            ips: ips.iter().map(|s| s.to_string()).collect(),
            manual_pairing_port: port,
        }
    }

    /// The two service views resolve independently and each sees a partial address
    /// set; a later resolve carrying only the link-local must not wipe out the
    /// routable address the other view found.
    #[test]
    fn merge_unions_addresses_across_views() {
        let devices = Arc::new(Mutex::new(HashMap::new()));
        merge_device(
            &devices,
            "k".into(),
            disc("Sam’s Apple Vision Pro", &["192.168.1.5"], Some(1234)),
        );
        // remotepairing view resolves later, catching only the link-local.
        merge_device(
            &devices,
            "k".into(),
            disc("Sams AppleVisionPro", &["169.254.7.9"], None),
        );
        let d = devices.lock().unwrap().get("k").cloned().unwrap();
        assert_eq!(d.ip, "192.168.1.5");
        assert_eq!(d.ips, vec!["192.168.1.5".to_string(), "169.254.7.9".to_string()]);
        assert_eq!(d.manual_pairing_port, Some(1234));
        assert_eq!(d.name, "Sam’s Apple Vision Pro");
    }

    /// An instant EHOSTUNREACH is macOS's Local Network filter refusing to send (the
    /// stuck-permission quirk); a slow one is an unanswered ARP (asleep headset). The
    /// two need opposite advice.
    #[test]
    fn unreachable_hint_depends_on_how_fast_it_failed() {
        let instant = std::io::Error::from_raw_os_error(65); // EHOSTUNREACH
        let m = connect_err_message("x", "192.168.50.93", 53231, instant, Some(Duration::from_millis(5)));
        assert!(m.contains("Local Network permission"), "{m}");
        let slow = std::io::Error::from_raw_os_error(65);
        let m = connect_err_message("x", "192.168.50.93", 53231, slow, Some(Duration::from_secs(4)));
        assert!(m.contains("asleep"), "{m}");
        // No timing info (e.g. mid-exchange socket drop): don't guess at policy.
        let unknown = std::io::Error::from_raw_os_error(65);
        let m = connect_err_message("x", "192.168.50.93", 53231, unknown, None);
        assert!(m.contains("asleep"), "{m}");
    }

    /// After a DHCP move the fresh routable address must be tried before the
    /// remembered (stale) one.
    #[test]
    fn merge_prefers_fresh_routable_over_remembered() {
        let devices = Arc::new(Mutex::new(HashMap::new()));
        merge_device(&devices, "k".into(), disc("VP", &["192.168.1.5"], Some(1)));
        merge_device(
            &devices,
            "k".into(),
            disc("VP", &["192.168.1.42", "169.254.7.9"], Some(2)),
        );
        let d = devices.lock().unwrap().get("k").cloned().unwrap();
        assert_eq!(
            d.ips,
            vec![
                "192.168.1.42".to_string(),
                "192.168.1.5".to_string(),
                "169.254.7.9".to_string()
            ]
        );
        assert_eq!(d.ip, "192.168.1.42");
        assert_eq!(d.manual_pairing_port, Some(2));
    }
}
