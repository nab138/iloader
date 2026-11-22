import Foundation

enum WorkspaceSection: String, CaseIterable, Identifiable {
    case home = "Overview"
    case devices = "Devices"
    case pairing = "Pairing files"
    case certificates = "Certificates"
    case appIDs = "App IDs"
    case settings = "Settings"

    var id: String { rawValue }

    var symbol: String {
        switch self {
        case .home: "square.grid.2x2"
        case .devices: "ipad"
        case .pairing: "key.horizontal"
        case .certificates: "checkmark.seal"
        case .appIDs: "app"
        case .settings: "gearshape"
        }
    }
}

struct PairingFile: Identifiable, Codable, Hashable {
    var id = UUID()
    let name: String
    let filename: String
    let importedAt: Date
    let byteCount: Int
}

struct CurrentDeviceInfo {
    let name: String
    let model: String
    let operatingSystem: String
}

enum LogSeverity: String, CaseIterable, Identifiable {
    case debug = "DEBUG"
    case info = "INFO"
    case warning = "WARN"
    case error = "ERROR"
    var id: String { rawValue }
}

struct LogEntry: Identifiable {
    let id = UUID()
    let date = Date()
    let severity: LogSeverity
    let message: String
}
