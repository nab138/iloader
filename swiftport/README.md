# iLoader SwiftPort (iPadOS)

Native SwiftUI rewrite of the iLoader app interface and the functions that can run inside an ordinary iPadOS app.

## Included
- Native iPad navigation and dashboard
- IPA file import into the app Documents container (storage/reference only)
- Pairing-file import, listing, sharing/export, and deletion within the app sandbox
- Settings and structured in-app logs
- Keychain helper for future supported local tokens
- Separate account, device-discovery, and sideload service boundaries
- Dedicated device, account, certificate, and App ID screens

## iPadOS capability boundary
The desktop backend uses Rust libraries such as idevice and isideload, plus usbmuxd/lockdown and desktop account/signing flows. An ordinary iPadOS app cannot simply run that desktop stack. iPadOS also does not let an app install arbitrary IPA files itself or write into another app's private container. Device discovery, installation, developer portal, certificate, and App ID screens therefore report that a compatible companion/service is required. No fake success state or fabricated device/account data is shown.

Pairing files can be imported and exported as user-selected documents, but importing a file is not the same as performing a device pairing handshake.

## Add to Xcode
Create an iOS App project named iLoaderSwiftPort, choose SwiftUI, set deployment target to iPadOS 17 or later, then add every Swift file in this folder to the app target. The app entry point is ILoaderApp.swift.

JSON locale files, SVG artwork, licenses, and build metadata are resources/documentation rather than executable scripts; they are not meaningfully converted into Swift source.
