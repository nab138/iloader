import SwiftUI
import UIKit
struct SettingsView: View {
 @EnvironmentObject private var state: AppState
 @State private var filter: LogSeverity = .debug
 private var filtered: [LogEntry] { let levels = LogSeverity.allCases; let index = levels.firstIndex(of: filter) ?? 0; return state.logs.filter { (levels.firstIndex(of: $0.severity) ?? 0) >= index } }
 var body: some View {
  Form {
   Section("Anisette server") {
    TextField("Server hostname", text: $state.anisetteServer).textInputAutocapitalization(.never).autocorrectionDisabled()
    Text("Saved as a preference only. Anisette requests are not active in this iPadOS build.").font(.footnote).foregroundStyle(.secondary)
   }
   Section("Security") {
    Toggle("Prefer Keychain storage", isOn: $state.useKeychain)
    Text("This port does not collect Apple ID credentials. Keychain support is provided for future supported local tokens.").font(.footnote).foregroundStyle(.secondary)
   }
   Section("Logs") {
    Picker("Minimum level", selection: $filter) { ForEach(LogSeverity.allCases) { Text($0.rawValue).tag($0) } }
    ForEach(filtered) { entry in VStack(alignment: .leading) { Text(entry.message); Text("[\(entry.severity.rawValue)] \(entry.date.formatted(date: .abbreviated, time: .standard))").font(.caption).foregroundStyle(.secondary) } }
    Button("Copy logs") { UIPasteboard.general.string = filtered.map { "[\($0.severity.rawValue)] \($0.message)" }.joined(separator: "\n") }
   }
   Section { Button("Save settings") { state.saveSettings() }; Text("iLoader SwiftPort • iPadOS").font(.caption).foregroundStyle(.secondary) }
  }.navigationTitle("Settings")
 }
}