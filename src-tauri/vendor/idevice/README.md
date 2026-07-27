# idevice

A pure Rust library for interacting with iOS services.
Inspired by [libimobiledevice](https://github.com/libimobiledevice/libimobiledevice)
[pymobiledevice3](https://github.com/doronz88/pymobiledevice3),
and [go-ios](https://github.com/danielpaulus/go-ios)
this library interfaces with lockdownd, usbmuxd, and RSD to perform actions
on an iOS device that a Mac normally would.

For help and information, join the [idevice Discord](https://discord.gg/qtgv6QtYbV)

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/jkcoxson/idevice)

## State

**IMPORTANT**: Breaking changes will happen at each point release until 0.2.0.
Pin your `Cargo.toml` to a specific version to avoid breakage.

This library is in development and research stage.
Releases are being published to crates.io for use in other projects,
but the API and feature-set are far from final or even planned.

## Why use this?

libimobiledevice is a groundbreaking library. Unfortunately, it hasn't
been seriously updated in a long time, and does not support many modern
iOS features.

Libraries such as pymobiledevice3 and go-ios have popped up to fill that
gap, but both lacked the support I needed for embedding into applications
and server programs. Python requires an interpreter, and Go's current
ability to be embedded in other languages is lacking.

This library is currently used in popular apps such as
[StikDebug](https://github.com/StephenDev0/StikDebug),
[CrossCode](https://github.com/nab138/CrossCode)
and
[Protokolle](https://github.com/khcrysalis/Protokolle).
``idevice`` has proven there is a need. It's currently deployed on tens of
thousands of devices, all across the world.

## Features

To keep dependency bloat and compile time down, everything is contained in features.

| Feature                | Description |
|------------------------|-----------------------------------------------------------------------------|
| `afc`                  | Apple File Conduit for file system access.|
| `amfi`                 | Apple mobile file integrity service |
| `bt_packet_logger`     | Capture Bluetooth packets. |
| `companion_proxy`      | Manage paired Apple Watches. |
| `core_device_proxy`    | Start a secure tunnel to access protected services. |
| `crashreportcopymobile`| Copy crash reports.|
| `debug_proxy`          | Send GDB commands to the device.|
| `diagnostics_relay`    | Access device diagnostics information (IORegistry, MobileGestalt, battery, NAND, device control).|
| `dvt`                  | Access Apple developer tools (e.g. Instruments).|
| `heartbeat`            | Maintain a heartbeat connection.|
| `house_arrest` | Manage files in app containers |
| `installation_proxy`   | Manage app installation and uninstallation.|
| `installcoordination_proxy` | Manage app installation coordination.|
| `location_simulation`  | Simulate GPS locations on the device.|
| `misagent`             | Manage provisioning profiles on the device.|
| `mobile_image_mounter` | Manage DDI images.|
| `mobileactivationd`    | Activate/Deactivate device.|
| `mobilebackup2`        | Manage backups.|
| `pair`                 | Pair the device.|
| `pcapd`                | Capture network packets.|
| `preboard_service`     | Interface with Preboard.|
| `restore_service`      | Restore service (recovery/reboot).|
| `screenshotr`          | Take screenshots.|
| `springboardservices`  | Control SpringBoard (icons, wallpaper, orientation, etc.).|
| `syslog_relay` | Relay system logs and OS trace logs from the device. |
| `tcp`                  | Connect to devices over TCP.|
| `tunnel_tcp_stack`     | Naive in-process TCP stack for `core_device_proxy`.|
| `tss`                  | Make requests to Apple's TSS servers. Partial support.|
| `tunneld`              | Interface with [pymobiledevice3](https://github.com/doronz88/pymobiledevice3)'s tunneld. |
| `usbmuxd`              | Connect using the usbmuxd daemon.|
| `xpc`                  | Access protected services via XPC over RSD. |
| `xctest`               | Launch XCTest runners and coordinate modern testmanagerd/XCTest sessions. |
| `wda`                  | Minimal WebDriverAgent bootstrap helpers and localhost bridge support. |
| `notification_proxy`   | Post and observe iOS notifications. |

### Planned/TODO

Finish the following:

- webinspector

Implement the following:

- file_relay

As this project is done in my free time within my busy schedule, there
is no ETA for any of these. Feel free to contribute or donate!

## Usage

idevice is purposefully verbose to allow for powerful configurations.
No size fits all, but effort is made to reduce boilerplate via providers.

```rust
// enable the usbmuxd feature
use idevice::{lockdown::LockdowndClient, IdeviceService};
use idevice::usbmuxd::{UsbmuxdAddr, UsbmuxdConnection},

#[tokio::main]
async fn main() {
    // usbmuxd is Apple's daemon for connecting to devices over USB.
    // We'll ask usbmuxd for a device
    let mut usbmuxd = UsbmuxdConnection::default()
        .await
        .expect("Unable to connect to usbmuxd");
    let devs = usbmuxd.get_devices().unwrap();
    if devs.is_empty() {
        eprintln!("No devices connected!");
        return;
    }

    // Create a provider to automatically create connections to the device.
    // Many services require opening multiple connections to get where you want.
    let provider = devs[0].to_provider(UsbmuxdAddr::from_env_var().unwrap(), 0, "example-program")

    // ``connect`` takes an object with the provider trait
    let mut lockdown_client = match LockdowndClient::connect(&provider).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Unable to connect to lockdown: {e:?}");
            return;
        }
    };

    println!("{:?}", lockdown_client.get_value("ProductVersion").await);
    println!(
        "{:?}",
        lockdown_client
            .start_session(
                &provider
                    .get_pairing_file()
                    .await
                    .expect("failed to get pairing file")
            )
            .await
    );
    println!("{:?}", lockdown_client.idevice.get_type().await.unwrap());
    println!("{:#?}", lockdown_client.get_all_values().await);
}
```

More examples are in the [`tools`](tools/) crate and in the crate documentation.

### XCTest / WDA

`idevice` also includes support for launching XCTest runners through the
library's modern DVT/RSD path. This can be used to start
WebDriverAgent-style runners on recent iOS versions.

The bundled CLI exposes this through the `idevice-tools xctest` command:

```bash
idevice-tools --udid <device-udid> xctest io.github.kor1k1.WebDriverAgentRunner.xctrunner
```

To wait for WDA and expose localhost bridge URLs for HTTP and MJPEG:

```bash
idevice-tools --udid <device-udid> xctest --bridge io.github.kor1k1.WebDriverAgentRunner.xctrunner
```

The current `wda` support is intentionally a bootstrap layer for readiness
checks and session startup, rather than a complete long-lived WebDriver
client.

## FFI

For use in other languages, a small FFI crate has been created to start exposing
idevice. Example C programs can be found in the [`ffi/examples`](ffi/examples/) directory.

### C++

"Hey wait a second, there's a lot of C++ code in this library!!"
C++ bindings have been made for many of idevice's features. This allows smooth
and safer usage in C++ and Swift codebases.

## Technical Explanation

There are so many layers and protocols in this library, many stacked on top of
one another. It's difficult to describe the magnitude that is Apple's interfaces.

I would recommend reading the DeepWiki explanations and overviews to get an idea
of how this library and their associated protocols work. But a general overview is:

### Lockdown

1. A lockdown service is accessible via a port given by lockdown
1. Lockdown is accessible by USB or TCP via TLS
1. USB is accessible via usbmuxd
1. usbmuxd is accessed through a unix socket
1. That Unix socket has its own protocol

### RemoteXPC/RSD

1. An RSD service is discovered through a RemoteXPC handshake response
1. RemoteXPC is transferred over non-compliant HTTP/2
1. That HTTP/2 is accessed through an NCM USB interface or CoreDeviceProxy
1. CoreDeviceProxy is a lockdown service, see above

For more information about rppairing, I've written out the [spec here](https://jkcoxson.com/blog/rppairing-spec)

## License

MIT
