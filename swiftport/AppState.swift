import Foundation
import SwiftUI
@MainActor final class AppState: ObservableObject {
 @Published var selectedSection: WorkspaceSection = .home
 @Published var importedIPAName: String?
 @Published var pairingFiles: [PairingFile] = []
 @Published var logs: [LogEntry] = []
 @Published var anisetteServer = UserDefaults.standard.string(forKey: "anisetteServer") ?? "mac-ani.jkcoxson.com"
 @Published var useKeychain = UserDefaults.standard.bool(forKey: "useKeychain")
 @Published var alertMessage = ""
 @Published var showingAlert = false
 init() { refreshPairingFiles(); log(.info, "iLoader iPadOS workspace started"); log(.warning, "Desktop USB and installation services are unavailable on iPadOS") }
 func log(_ severity: LogSeverity, _ message: String) { logs.insert(LogEntry(severity: severity, message: message), at: 0); if logs.count > 500 { logs.removeLast(logs.count - 500) } }
 func refreshPairingFiles() { pairingFiles = PairingService.shared.listFiles() }
 func reportUnavailable(_ feature: String, reason: String) { alertMessage = "\(feature): \(reason)"; showingAlert = true; log(.warning, alertMessage) }
 func saveSettings() { UserDefaults.standard.set(anisetteServer, forKey: "anisetteServer"); UserDefaults.standard.set(useKeychain, forKey: "useKeychain"); log(.info, "Settings saved") }
}