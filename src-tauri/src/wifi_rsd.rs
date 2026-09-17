use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    time::Duration,
};

use idevice::{
    Idevice, IdeviceError, IdeviceService,
    provider::IdeviceProvider,
    remote_pairing::{
        RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native,
    },
    rsd::RsdHandshake,
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tauri::{AppHandle, Manager};
use tokio::net::TcpStream;
use tracing::{info, warn};

use crate::{device::DeviceInfo, error::AppError};

const REMOTE_PAIRING_SERVICE: &str = "_remotepairing._tcp.local.";
const REMOTE_PAIRING_HOST: &str = "iloader";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct RemotePairingLockdownServiceCompat {
    idevice: Idevice,
}

impl IdeviceService for RemotePairingLockdownServiceCompat {
    fn service_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("com.apple.dt.remotepairingdeviced.lockdown")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, IdeviceError> {
        Ok(Self { idevice })
    }
}

fn remote_error(context: &str, error: impl std::fmt::Display) -> AppError {
    AppError::RemotePairing(format!("{context}: {error}"))
}

fn remote_pairing_path(app: &AppHandle, udid: &str) -> Result<PathBuf, AppError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| {
            AppError::Filesystem(
                "Failed to get app data directory for RemotePairing".into(),
                e.to_string(),
            )
        })?
        .join("remote-pairing");

    std::fs::create_dir_all(&dir).map_err(|e| {
        AppError::Filesystem(
            "Failed to create RemotePairing directory".into(),
            e.to_string(),
        )
    })?;

    Ok(dir.join(format!("{udid}.plist")))
}

pub fn known_remote_pairing_udids(app: &AppHandle) -> Result<Vec<String>, AppError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| {
            AppError::Filesystem(
                "Failed to get app data directory for RemotePairing discovery".into(),
                e.to_string(),
            )
        })?
        .join("remote-pairing");

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(&dir).map_err(|e| {
        AppError::Filesystem(
            "Failed to read RemotePairing directory".into(),
            e.to_string(),
        )
    })?;

    let mut udids = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Ignoring unreadable RemotePairing directory entry: {e}");
                continue;
            }
        };

        let path = entry.path();

        if path.extension().and_then(|value| value.to_str()) != Some("plist") {
            continue;
        }

        if let Some(udid) = path.file_stem().and_then(|value| value.to_str())
            && !udid.is_empty()
        {
            udids.push(udid.to_string());
        }
    }

    Ok(udids)
}
async fn connect_tcp(address: Ipv4Addr, port: u16, context: &str) -> Result<TcpStream, AppError> {
    tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((address, port)))
        .await
        .map_err(|e| remote_error(&format!("{context} timed out"), e))?
        .map_err(|e| remote_error(context, e))
}

async fn discover_remote_pairing_port(target: Ipv4Addr) -> Result<u16, AppError> {
    let mdns = ServiceDaemon::new()
        .map_err(|e| remote_error("Failed to start RemotePairing mDNS discovery", e))?;

    let receiver = mdns
        .browse(REMOTE_PAIRING_SERVICE)
        .map_err(|e| remote_error("Failed to browse RemotePairing service", e))?;

    let deadline = tokio::time::Instant::now() + DISCOVERY_TIMEOUT;
    let mut found = None;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, receiver.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(service))) => {
                let matches = service
                    .get_addresses_v4()
                    .into_iter()
                    .any(|candidate| candidate == target);

                if matches {
                    found = Some(service.get_port());
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }

    let _ = mdns.stop_browse(REMOTE_PAIRING_SERVICE);
    let _ = mdns.shutdown();

    found.ok_or_else(|| {
        AppError::RemotePairing(format!(
            "No _remotepairing._tcp service found for Wi-Fi device {target}"
        ))
    })
}

pub async fn bootstrap_remote_pairing(
    app: &AppHandle,
    device: &DeviceInfo,
    provider: &impl IdeviceProvider,
) -> Result<(), AppError> {
    if device.connection_type != "USB" {
        return Ok(());
    }

    let path = remote_pairing_path(app, &device.udid)?;

    let mut pairing = match RpPairingFile::read_from_file(&path).await {
        Ok(pairing) => pairing,
        Err(_) => RpPairingFile::generate(REMOTE_PAIRING_HOST),
    };

    let service = RemotePairingLockdownServiceCompat::connect(provider)
        .await
        .map_err(|e| remote_error("Failed to open USB RemotePairing control service", e))?;

    let socket = service.idevice.get_socket().ok_or_else(|| {
        AppError::RemotePairing("USB RemotePairing service returned no socket".into())
    })?;

    let mut client = RemotePairingClient::new(RpPairingSocket::new(socket), REMOTE_PAIRING_HOST);

    client
        .connect(&mut pairing, || async { "000000".to_string() })
        .await
        .map_err(|e| remote_error("Failed to bootstrap RemotePairing over USB", e))?;

    pairing
        .write_to_file(&path)
        .await
        .map_err(|e| remote_error("Failed to persist RemotePairing record", e))?;

    info!(
        "RemotePairing identity prepared for {} ({})",
        device.name, device.udid
    );

    Ok(())
}

pub async fn open_rsd_tunnel(
    app: &AppHandle,
    device: &DeviceInfo,
) -> Result<(AdapterHandle, RsdHandshake), AppError> {
    let network_address = device.network_address.as_deref().ok_or_else(|| {
        AppError::RemotePairing("Selected network device has no Wi-Fi address".into())
    })?;

    let address = network_address.parse::<Ipv4Addr>().map_err(|e| {
        AppError::RemotePairing(format!(
            "Invalid RemotePairing IPv4 address {network_address}: {e}"
        ))
    })?;

    let pairing_path = remote_pairing_path(app, &device.udid)?;
    let mut pairing = RpPairingFile::read_from_file(&pairing_path)
        .await
        .map_err(|e| {
            remote_error(
                "No valid iLoader RemotePairing record; reconnect the iPhone over USB once",
                e,
            )
        })?;

    let pairing_port = discover_remote_pairing_port(address).await?;
    info!(
        "Opening RemotePairing transport for {} at {}:{}",
        device.udid, address, pairing_port
    );

    let pairing_stream = connect_tcp(
        address,
        pairing_port,
        "Failed to connect to RemotePairing service",
    )
    .await?;

    let pairing_socket = RpPairingSocket::new(pairing_stream);
    let mut client = RemotePairingClient::new(pairing_socket, REMOTE_PAIRING_HOST);

    client
        .attempt_pair_verify()
        .await
        .map_err(|e| remote_error("RemotePairing handshake failed", e))?;

    client.validate_pairing(&mut pairing).await.map_err(|e| {
        remote_error(
            "RemotePairing pair-verify failed; reconnect the iPhone over USB once",
            e,
        )
    })?;

    let tunnel_port = client
        .create_tcp_listener()
        .await
        .map_err(|e| remote_error("Failed to create RemotePairing TCP tunnel listener", e))?;

    let tunnel_stream = connect_tcp(
        address,
        tunnel_port,
        "Failed to connect to RemotePairing tunnel",
    )
    .await?;

    let tunnel = connect_tls_psk_tunnel_native(tunnel_stream, client.encryption_key())
        .await
        .map_err(|e| remote_error("TLS-PSK/CDTunnel handshake failed", e))?;

    let client_ip = tunnel
        .info
        .client_address
        .parse::<IpAddr>()
        .map_err(|e| remote_error("Invalid CDTunnel client address", e))?;

    let server_ip = tunnel
        .info
        .server_address
        .parse::<IpAddr>()
        .map_err(|e| remote_error("Invalid CDTunnel server address", e))?;

    let rsd_port = tunnel.info.server_rsd_port;
    let mtu = tunnel.info.mtu as usize;

    info!(
        "RemotePairing CDTunnel established for {} with RSD port {}",
        device.udid, rsd_port
    );

    let raw = tunnel.into_inner();
    let mut adapter = Adapter::new(Box::new(raw), client_ip, server_ip);
    adapter.set_mss(mtu.saturating_sub(60));

    let mut provider = adapter.to_async_handle();

    let rsd_stream = provider
        .connect(rsd_port)
        .await
        .map_err(|e| remote_error("Failed to connect to RSD through userspace tunnel", e))?;

    let handshake = RsdHandshake::new(rsd_stream)
        .await
        .map_err(|e| remote_error("RSD handshake failed", e))?;

    info!(
        "RSD ready for {} with {} services",
        device.udid,
        handshake.services.len()
    );

    Ok((provider, handshake))
}
