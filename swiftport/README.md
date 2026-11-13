# iLoader SwiftPort (iPadOS)

A native SwiftUI iPadOS workspace based on the desktop iLoader project. The target is the iPad running the app itself.

## Implemented in this branch

- Native iPad navigation and dashboard.
- Automatic identification of the current iPad using public UIKit properties (device name, model family, and OS version); no serial number or UDID collection.
- IPA document import into the app's private Documents directory (storage/reference only).
- Pairing-file import, listing, sharing/export, and deletion inside the app sandbox.
- Keychain-backed storage for a PKCS#12 identity.
- PKCS#12 import validation using Security.framework: the imported file must contain a certificate and its matching private key, and the supplied password must be correct.
- PKCS#12 export and removal from the Certificates screen.
- Settings and structured in-app logs.
- A macOS GitHub Actions workflow that builds the iOS Simulator target without code signing.

## Certificate behavior and current limitation

The Certificates screen can import a real existing `.p12` identity, securely store the original password-protected file in the iPad Keychain, and export that same identity. The future certificate-generation backend must save its completed PKCS#12 bytes through `SigningIdentityStore.importPKCS12(_:password:)` or an equivalent validated save method.

**This branch does not yet perform Apple developer login, create a CSR, request a development certificate, generate a provisioning profile, or create a new PKCS#12 bundle.** The original desktop code performs those steps inside the external Rust `isideload` crate. They have not been translated into a functioning iPadOS implementation. The iOS Security API can import PKCS#12 identities, but [Apple DTS confirms that iOS has no API to export a Keychain digital identity to PKCS#12](https://developer.apple.com/forums/thread/756991); therefore the iPad app must preserve/create the PKCS#12 data itself or use a supported companion implementation. The original `isideload` implementation's `CertificateIdentity.as_p12()` uses the machine ID as the password for SideStore/AltStore compatibility, so a future native generator must preserve that compatibility behavior.

[Apple's official App Store Connect API supports certificate creation from a CSR](https://developer.apple.com/documentation/appstoreconnectapi/create-a-certificate) for eligible developer teams using its API authentication model. It is not the same as the desktop project's Apple-ID/Anisette private authentication flow and is not a drop-in replacement for free-account sign-in.

## iPadOS capability boundaries

The desktop backend uses Rust libraries such as `idevice` and `isideload`, plus desktop `usbmuxd`/lockdown and account/signing flows. An ordinary iPadOS app cannot simply run that desktop stack, inspect arbitrary other app containers, or install arbitrary IPA files itself. The app can identify itself and handle user-selected files, but external-device management and arbitrary IPA installation need supported Apple platform capabilities or a compatible companion.

Importing a pairing file is not the same as performing a device pairing handshake. Importing an IPA is not signing or installing it.

## Build

Open `swiftport/iLoaderSwiftPort.xcodeproj` in Xcode 16 or later. The project targets iPadOS 17 or later and supports iPad device family 2. GitHub Actions builds the iOS Simulator target on pushes to `swiftport-ipados`.

No signing credentials or Apple account passwords are stored in the repository. Never commit private keys, provisioning profiles, Apple account credentials, or exported `.p12` files.
