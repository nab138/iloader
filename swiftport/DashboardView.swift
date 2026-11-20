import SwiftUI
import UniformTypeIdentifiers
import UIKit

struct DashboardView: View {
    @EnvironmentObject private var state: AppState
    @State private var importingIPA = false

    private var currentDevice: CurrentDeviceInfo {
        DeviceService().currentDevice()
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                HStack {
                    VStack(alignment: .leading) {
                        Text("iLoader").font(.largeTitle.bold())
                        Text("Native iPadOS workspace").foregroundStyle(.secondary)
                    }
                    Spacer()
                    Image(systemName: "shippingbox.fill")
                        .font(.system(size: 36))
                        .foregroundStyle(.tint)
                }

                HStack(spacing: 12) {
                    card("This device", currentDevice.model, "ipad")
                    card("Pairing files", "\(state.pairingFiles.count)", "key.horizontal")
                    card("Logs", "\(state.logs.count)", "text.alignleft")
                }

                GroupBox("Current iPad") {
                    VStack(alignment: .leading, spacing: 6) {
                        Label(currentDevice.name, systemImage: "ipad")
                            .font(.headline)
                        Text(currentDevice.operatingSystem)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                AccountView()

                GroupBox("IPA workspace") {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(state.importedIPAName ?? "Choose an IPA file to keep in this app's sandbox.")
                            .foregroundStyle(.secondary)
                        Button {
                            importingIPA = true
                        } label: {
                            Label("Import IPA file", systemImage: "square.and.arrow.down")
                        }
                        .buttonStyle(.borderedProminent)
                        Text("Importing stores the file for reference. iPadOS does not allow this app to install arbitrary IPA files itself.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                GroupBox("Platform capability") {
                    VStack(alignment: .leading, spacing: 8) {
                        Label("Automatic detection of this iPad is supported.", systemImage: "checkmark.circle.fill")
                        Label("Pairing documents and IPA files can be imported/exported.", systemImage: "checkmark.circle.fill")
                        Label("External USB management and arbitrary IPA installation require supported Apple platform capabilities or a companion.", systemImage: "info.circle")
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(24)
        }
        .navigationTitle("Overview")
        .fileImporter(
            isPresented: $importingIPA,
            allowedContentTypes: [UTType(filenameExtension: "ipa") ?? .data]
        ) { result in
            do {
                let saved = try FileImportService.shared.copyIntoDocuments(result.get())
                state.importedIPAName = saved.lastPathComponent
                state.log(.info, "Imported IPA: \(saved.lastPathComponent)")
            } catch {
                state.reportUnavailable("IPA import", reason: error.localizedDescription)
            }
        }
    }

    private func card(_ title: String, _ value: String, _ symbol: String) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Image(systemName: symbol).foregroundStyle(.tint)
                Text(value).font(.title2.bold())
                Text(title).font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
