import Foundation
enum PlatformServices {
 static let deviceDiscoveryAvailable = false
 static let directIPASigningAvailable = false
 static let directAppInstallationAvailable = false
 static let developerPortalManagementAvailable = false
 static let pairingFileDocumentImportAvailable = true
 static let ipaDocumentImportAvailable = true
 static let explanation = "iPadOS apps are sandboxed and cannot run the desktop usbmuxd/lockdown stack or install arbitrary IPA files themselves. Desktop device operations require a supported external companion or service."
}