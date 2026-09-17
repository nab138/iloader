use std::{
    collections::HashSet,
    future::Future,
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
    sync::Mutex,
    time::Duration,
};

use idevice::{
    Idevice, IdeviceError, IdeviceService,
    lockdown::LockdownClient,
    pairing_file::PairingFile,
    provider::{IdeviceProvider, TcpProvider, UsbmuxdProvider},
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    error::AppError,
    pairing::pairing_file,
    wifi_rsd::{bootstrap_remote_pairing, known_remote_pairing_udids},
};

const MOBDEV2_SERVICE: &str = "_apple-mobdev2._tcp.local.";
const WIFI_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1500);
const WIFI_CONNECT_TIMEOUT: Duration = Duration::from_millis(1000);

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub id: u32,
    pub udid: String,
    pub connection_type: String,
    pub version: String,
    #[serde(default)]
    pub network_address: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfoWithPairing {
    pub info: DeviceInfo,
    pub pairing: Vec<u8>,
}

pub type DeviceInfoMutex = Mutex<Option<DeviceInfoWithPairing>>;
pub type PairingCancelToken = Mutex<Option<CancellationToken>>;

#[derive(Debug)]
pub enum DeviceProvider {
    Usbmuxd(UsbmuxdProvider),
    Tcp(TcpProvider),
}

impl IdeviceProvider for DeviceProvider {
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>> {
        match self {
            Self::Usbmuxd(provider) => provider.connect(port),
            Self::Tcp(provider) => provider.connect(port),
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Usbmuxd(provider) => provider.label(),
            Self::Tcp(provider) => provider.label(),
        }
    }

    fn get_pairing_file(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PairingFile, IdeviceError>> + Send>> {
        match self {
            Self::Usbmuxd(provider) => provider.get_pairing_file(),
            Self::Tcp(provider) => provider.get_pairing_file(),
        }
    }
}

async fn discover_wifi_addresses() -> Result<Vec<Ipv4Addr>, AppError> {
    let mdns = ServiceDaemon::new().map_err(|e| {
        AppError::DeviceComsWithMessage(
            "Failed to start Bonjour/mDNS discovery".into(),
            e.to_string(),
        )
    })?;
    let receiver = mdns.browse(MOBDEV2_SERVICE).map_err(|e| {
        AppError::DeviceComsWithMessage(
            "Failed to browse for Wi-Fi iOS devices".into(),
            e.to_string(),
        )
    })?;

    let deadline = tokio::time::Instant::now() + WIFI_DISCOVERY_TIMEOUT;
    let mut addresses = HashSet::new();

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, receiver.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(service))) => {
                addresses.extend(service.get_addresses_v4());
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                warn!("Bonjour/mDNS receiver stopped: {e}");
                break;
            }
            Err(_) => break,
        }
    }

    let _ = mdns.stop_browse(MOBDEV2_SERVICE);
    let _ = mdns.shutdown();
    Ok(addresses.into_iter().collect())
}

async fn inspect_wifi_device(
    addr: Ipv4Addr,
    pairing_file: &PairingFile,
) -> Result<DeviceInfo, AppError> {
    let stream = tokio::time::timeout(
        WIFI_CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect((addr, LockdownClient::LOCKDOWND_PORT)),
    )
    .await
    .map_err(|e| {
        AppError::DeviceComsWithMessage(
            format!("Timed out connecting to Wi-Fi device {addr}"),
            e.to_string(),
        )
    })?
    .map_err(|e| {
        AppError::DeviceComsWithMessage(
            format!("Failed to connect to Wi-Fi device {addr}"),
            e.to_string(),
        )
    })?;

    let idevice = Idevice::new(Box::new(stream), "iloader".to_string());
    let mut lockdown_client = LockdownClient::new(idevice);

    lockdown_client
        .start_session(pairing_file)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                format!("Failed to authenticate Wi-Fi device {addr}"),
                e.to_string(),
            )
        })?;

    let udid_value = lockdown_client
        .get_value(Some("UniqueDeviceID"), None)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                format!("Failed to read UDID from Wi-Fi device {addr}"),
                e.to_string(),
            )
        })?;
    let udid = udid_value
        .as_string()
        .ok_or_else(|| AppError::DeviceComs("Wi-Fi device UDID was not a string".into()))?;

    let name_value = lockdown_client
        .get_value(Some("DeviceName"), None)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                format!("Failed to read name from Wi-Fi device {addr}"),
                e.to_string(),
            )
        })?;
    let name = name_value
        .as_string()
        .ok_or_else(|| AppError::DeviceComs("Wi-Fi device name was not a string".into()))?;

    let version_value = lockdown_client
        .get_value(Some("ProductVersion"), None)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                format!("Failed to read version from Wi-Fi device {addr}"),
                e.to_string(),
            )
        })?;
    let version = version_value
        .as_string()
        .ok_or_else(|| AppError::DeviceComs("Product version was not a string".into()))?;

    Ok(DeviceInfo {
        name: name.to_string(),
        id: u32::from_be_bytes(addr.octets()),
        udid: udid.to_string(),
        connection_type: "Network".to_string(),
        version: version.to_string(),
        network_address: Some(addr.to_string()),
    })
}

async fn enable_wifi_connections(
    device: &DeviceInfo,
    usbmuxd: &mut UsbmuxdConnection,
) -> Result<(), AppError> {
    if device.connection_type != "USB" {
        return Ok(());
    }

    let provider = get_provider_from_connection(device, usbmuxd).await?;
    let mut pairing_file = usbmuxd.get_pair_record(&device.udid).await.map_err(|e| {
        AppError::LockdownPairing(
            "Failed to get pairing record while enabling Wi-Fi connections".into(),
            e.to_string(),
        )
    })?;
    pairing_file.udid = Some(device.udid.clone());

    let mut lockdown_client = LockdownClient::connect(&provider).await.map_err(|e| {
        AppError::DeviceComsWithMessage(
            "Failed to connect to lockdown while enabling Wi-Fi connections".into(),
            e.to_string(),
        )
    })?;

    lockdown_client
        .start_session(&pairing_file)
        .await
        .map_err(|e| {
            AppError::DeviceComsWithMessage(
                "Failed to start lockdown session while enabling Wi-Fi connections".into(),
                e.to_string(),
            )
        })?;

    lockdown_client
        .set_value(
            "EnableWifiConnections",
            true.into(),
            Some("com.apple.mobile.wireless_lockdown"),
        )
        .await
        .map_err(|e| {
            AppError::LockdownPairing("Failed to enable Wi-Fi connections".into(), e.to_string())
        })?;

    info!(
        "Enabled wireless lockdown connections for {} ({})",
        device.name, device.udid
    );
    Ok(())
}

#[tauri::command]
pub async fn list_devices(
    app: AppHandle,
    device_state: State<'_, DeviceInfoMutex>,
) -> Result<Vec<Result<DeviceInfo, AppError>>, AppError> {
    let mut usbmuxd = get_usbmuxd().await?;

    let selected_udid = {
        let guard = device_state.lock().unwrap();
        guard.as_ref().map(|selected| selected.info.udid.clone())
    };

    let mut known_udids: HashSet<String> = known_remote_pairing_udids(&app)?.into_iter().collect();

    if let Some(udid) = selected_udid.as_ref() {
        known_udids.insert(udid.clone());
    }

    let mut known_pairings = Vec::new();

    for udid in known_udids {
        match usbmuxd.get_pair_record(&udid).await {
            Ok(mut pairing_file) => {
                pairing_file.udid = Some(udid.clone());
                known_pairings.push((udid, pairing_file));
            }
            Err(e) => {
                warn!(
                    "Unable to load lockdown pairing record for known Wi-Fi device {}: {}",
                    udid, e
                );
            }
        }
    }
    let devs = usbmuxd.get_devices().await.map_err(|e| {
        AppError::Usbmuxd("Failed to list devices from usbmuxd".into(), e.to_string())
    })?;

    let usbmuxd_addr = UsbmuxdAddr::from_env_var().map_err(|e| {
        AppError::Usbmuxd(
            "Invalid usbmuxd address from environment".into(),
            e.to_string(),
        )
    })?;

    let device_info_futures: Vec<_> = devs
        .iter()
        .map(|d| {
            let usbmuxd_addr = usbmuxd_addr.clone();
            async move {
                let provider = d.to_provider(usbmuxd_addr, "iloader");
                let device_uid = d.device_id;
                let connection_type = match d.connection_type {
                    Connection::Usb => "USB",
                    Connection::Network(_) => "Network",
                    Connection::Unknown(_) => "Unknown",
                }
                .to_string();

                let mut lockdown_client =
                    LockdownClient::connect(&provider).await.map_err(|e| {
                        eprintln!("Unable to connect to lockdown for {}: {e:?}", d.udid);
                        AppError::DeviceComsWithMessage(
                            "Unable to connect to lockdown".into(),
                            e.to_string(),
                        )
                    })?;

                let device_name_value = lockdown_client
                    .get_value(Some("DeviceName"), None)
                    .await
                    .map_err(|e| {
                    eprintln!("Failed to fetch DeviceName for {}: {e:?}", d.udid);
                    AppError::DeviceComsWithMessage(
                        "Failed to fetch DeviceName".into(),
                        e.to_string(),
                    )
                })?;

                let device_name = device_name_value.as_string().ok_or_else(|| {
                    eprintln!("DeviceName for {} was not a string", d.udid);
                    AppError::DeviceComs("DeviceName was not a string".into())
                })?;

                let version_value = lockdown_client
                    .get_value(Some("ProductVersion"), None)
                    .await
                    .map_err(|e| {
                        eprintln!("Failed to fetch ProductVersion for {}: {e:?}", d.udid);
                        AppError::DeviceComsWithMessage(
                            "Failed to fetch ProductVersion".into(),
                            e.to_string(),
                        )
                    })?;

                let version = version_value.as_string().ok_or_else(|| {
                    eprintln!("ProductVersion for {} was not a string", d.udid);
                    AppError::DeviceComs("Product version was not a string".into())
                })?;

                Ok::<DeviceInfo, AppError>(DeviceInfo {
                    name: device_name.to_string(),
                    id: device_uid,
                    udid: d.udid.clone(),
                    connection_type,
                    version: version.to_string(),
                    network_address: None,
                })
            }
        })
        .collect();

    let mut device_infos = futures::future::join_all(device_info_futures).await;
    let mut network_udids: HashSet<String> = device_infos
        .iter()
        .filter_map(|result| match result {
            Ok(device) if device.connection_type == "Network" => Some(device.udid.clone()),
            _ => None,
        })
        .collect();

    if !known_pairings.is_empty() {
        match discover_wifi_addresses().await {
            Ok(addresses) => {
                for address in addresses {
                    let mut matched = false;

                    for (expected_udid, pairing_file) in &known_pairings {
                        let device = match inspect_wifi_device(address, pairing_file).await {
                            Ok(device) => device,
                            Err(_) => continue,
                        };

                        if device.udid != *expected_udid {
                            continue;
                        }

                        matched = true;

                        if network_udids.insert(device.udid.clone()) {
                            info!(
                                "Discovered known Wi-Fi device {} ({}) at {}",
                                device.name, device.udid, address
                            );
                            device_infos.push(Ok(device));
                        }

                        break;
                    }

                    if !matched {
                        warn!(
                            "Ignoring Wi-Fi candidate {} because no known pairing record matched",
                            address
                        );
                    }
                }
            }
            Err(e) => warn!("Unable to discover Wi-Fi devices with Bonjour/mDNS: {e}"),
        }
    }
    Ok(device_infos)
}

#[tauri::command]
pub async fn set_selected_device(
    app: AppHandle,
    device_state: State<'_, DeviceInfoMutex>,
    cancel_state: State<'_, PairingCancelToken>,
    device: Option<DeviceInfo>,
) -> Result<(), AppError> {
    Box::pin(set_selected_device_impl(
        app,
        device_state,
        cancel_state,
        device,
    ))
    .await
}

async fn set_selected_device_impl(
    app: AppHandle,
    device_state: State<'_, DeviceInfoMutex>,
    cancel_state: State<'_, PairingCancelToken>,
    device: Option<DeviceInfo>,
) -> Result<(), AppError> {
    if device.is_none() {
        let mut device_state = device_state.lock().unwrap();
        *device_state = None;
        return Ok(());
    }

    if let Some(next_device) = device.as_ref() {
        let existing_pairing = {
            let guard = device_state.lock().unwrap();
            guard
                .as_ref()
                .filter(|current| current.info.udid == next_device.udid)
                .map(|current| current.pairing.clone())
        };

        if let Some(pairing) = existing_pairing {
            info!(
                "Reusing existing pairing while switching {} to {} transport",
                next_device.udid, next_device.connection_type
            );
            let mut guard = device_state.lock().unwrap();
            *guard = Some(DeviceInfoWithPairing {
                info: next_device.clone(),
                pairing,
            });
            return Ok(());
        }
    }

    let mut usbmuxd = get_usbmuxd().await?;

    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut guard = cancel_state.lock().unwrap();
        if let Some(old) = guard.replace(token.clone()) {
            old.cancel();
        }
    }

    let pairing_result =
        pairing_file(&app, device.as_ref().unwrap(), &mut usbmuxd, token.clone()).await;

    if !token.is_cancelled() {
        let mut guard = cancel_state.lock().unwrap();
        *guard = None;
    }

    let pairing = pairing_result?;

    if let Some(selected) = device.as_ref()
        && let Err(e) = enable_wifi_connections(selected, &mut usbmuxd).await
    {
        warn!(
            "Unable to enable wireless lockdown connections for {} ({}): {}",
            selected.name, selected.udid, e
        );
    }

    if let Some(selected) = device.as_ref()
        && selected.connection_type == "USB"
    {
        match get_provider_from_connection(selected, &mut usbmuxd).await {
            Ok(provider) => {
                if let Err(e) = bootstrap_remote_pairing(&app, selected, &provider).await {
                    warn!(
                        "Unable to prepare RemotePairing for {} ({}): {}",
                        selected.name, selected.udid, e
                    );
                }
            }
            Err(e) => {
                warn!(
                    "Unable to reopen USB provider for RemotePairing bootstrap on {} ({}): {}",
                    selected.name, selected.udid, e
                );
            }
        }
    }

    let device_with_pairing = DeviceInfoWithPairing {
        info: device.unwrap(),
        pairing,
    };
    let mut device_state = device_state.lock().unwrap();
    *device_state = Some(device_with_pairing);
    Ok(())
}

#[tauri::command]
pub async fn cancel_pairing(cancel_state: State<'_, PairingCancelToken>) -> Result<(), AppError> {
    let mut guard = cancel_state.lock().unwrap();
    if let Some(token) = guard.take() {
        token.cancel();
    }
    Ok(())
}

pub async fn get_usbmuxd() -> Result<UsbmuxdConnection, AppError> {
    UsbmuxdConnection::default()
        .await
        .map_err(|e| AppError::Usbmuxd("Failed to connect to usbmuxd".into(), e.to_string()))
}

pub async fn get_provider(device_info: &DeviceInfo) -> Result<DeviceProvider, AppError> {
    get_provider_from_connection(device_info, &mut (get_usbmuxd().await?)).await
}

pub async fn get_provider_from_connection(
    device_info: &DeviceInfo,
    connection: &mut UsbmuxdConnection,
) -> Result<DeviceProvider, AppError> {
    if let Some(network_address) = device_info.network_address.as_deref() {
        let addr = network_address.parse::<IpAddr>().map_err(|e| {
            AppError::DeviceComsWithMessage("Invalid Wi-Fi device address".into(), e.to_string())
        })?;
        let mut pairing_file = connection
            .get_pair_record(&device_info.udid)
            .await
            .map_err(|e| {
                AppError::LockdownPairing(
                    "Failed to get pairing record for Wi-Fi device".into(),
                    e.to_string(),
                )
            })?;
        pairing_file.udid = Some(device_info.udid.clone());

        info!(
            "Using direct Wi-Fi lockdown transport for {} at {}",
            device_info.udid, addr
        );
        return Ok(DeviceProvider::Tcp(TcpProvider {
            addr,
            scope_id: None,
            pairing_file,
            label: "iloader".to_string(),
        }));
    }

    let devices = connection.get_devices().await.map_err(|e| {
        AppError::DeviceComsWithMessage("Failed to list devices".into(), e.to_string())
    })?;

    let mut exact = None;
    let mut same_udid = None;

    for device in devices {
        if device.udid != device_info.udid {
            continue;
        }

        if device.device_id == device_info.id {
            exact = Some(device);
            break;
        }

        if same_udid.is_none() {
            same_udid = Some(device);
        }
    }

    let device = exact.or(same_udid).ok_or_else(|| {
        AppError::DeviceComsWithMessage(
            "Selected device connection is no longer available".into(),
            format!(
                "No usbmuxd connection is available for {} ({})",
                device_info.udid, device_info.connection_type
            ),
        )
    })?;

    if device.device_id != device_info.id {
        info!(
            "Device {} changed usbmuxd connection id {} -> {}; continuing on the same UDID",
            device_info.udid, device_info.id, device.device_id
        );
    }

    let provider = device.to_provider(UsbmuxdAddr::from_env_var().unwrap(), "iloader");
    Ok(DeviceProvider::Usbmuxd(provider))
}
