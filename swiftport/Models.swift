import Foundation
enum WorkspaceSection: String, CaseIterable, Identifiable {
 case home = "Overview", devices = "Devices", pairing = "Pairing files", certificates = "Certificates", appIDs = "App IDs", settings = "Settings"
 var id: String { rawValue }
 var symbol: String { switch self { case .home: "square.grid.2x2"; case .devices: "ipad"; case .pairing: "key.horizontal"; case .certificates: "checkmark.seal"; case .appIDs: "app"; case .settings: "gearshape" } }
}
struct PairingFile: Identifiable, Codable, Hashable {
 var id = UUID(); let name: String; let filename: String; let importedAt: Date; let byteCount: Int
}
enum LogSeverity: String, CaseIterable, Identifiable { case debug = "DEBUG", info = "INFO", warning = "WARN", error = "ERROR"; var id: String { rawValue } }
struct LogEntry: Identifiable { let id = UUID(); let date = Date(); let severity: LogSeverity; let message: String }
