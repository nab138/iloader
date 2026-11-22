import SwiftUI
import UniformTypeIdentifiers
struct PairingView: View {
 @EnvironmentObject private var state: AppState
 @State private var importing = false
 var body: some View {
  List {
   Section { Text("Pairing files can be imported, shared, and deleted inside this app. This does not perform a device pairing handshake or place files inside another app's private container.").font(.footnote).foregroundStyle(.secondary)
    Button { importing = true } label: { Label("Import pairing file", systemImage: "square.and.arrow.down") }
   }
   Section("Stored pairing files") {
    if state.pairingFiles.isEmpty { ContentUnavailableView("No pairing files", systemImage: "key.horizontal", description: Text("Import a pairing file using the button above.")) }
    ForEach(state.pairingFiles) { file in
     HStack {
      VStack(alignment: .leading) { Text(file.name).font(.headline); Text(ByteCountFormatter.string(fromByteCount: Int64(file.byteCount), countStyle: .file)).font(.caption).foregroundStyle(.secondary) }
      Spacer()
      ShareLink(item: PairingService.shared.url(for: file)) { Image(systemName: "square.and.arrow.up") }
      Button(role: .destructive) { do { try PairingService.shared.delete(file); state.refreshPairingFiles(); state.log(.info, "Deleted \(file.filename)") } catch { state.reportUnavailable("Delete pairing file", reason: error.localizedDescription) } } label: { Image(systemName: "trash") }
     }
    }
   }
  }.navigationTitle("Pairing files")
  .fileImporter(isPresented: $importing, allowedContentTypes: [.data, .propertyList, UTType(filenameExtension: "mobiledevicepairing") ?? .data]) { result in
   do { let item = try PairingService.shared.importFile(from: result.get()); state.refreshPairingFiles(); state.log(.info, "Imported \(item.filename)") }
   catch { state.reportUnavailable("Pairing import", reason: error.localizedDescription) }
  }
 }
}