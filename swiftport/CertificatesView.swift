import SwiftUI
import UniformTypeIdentifiers

struct PKCS12Document: FileDocument {
    static var readableContentTypes: [UTType] { [.pkcs12] }
    var data: Data

    init(data: Data) { self.data = data }

    init(configuration: ReadConfiguration) throws {
        guard let contents = configuration.file.regularFileContents else {
            throw CocoaError(.fileReadCorruptFile)
        }
        data = contents
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

struct CertificatesView: View {
    @EnvironmentObject private var state: AppState
    @State private var exportDocument = PKCS12Document(data: Data())
    @State private var isExporting = false
    @State private var isImporting = false
    @State private var hasIdentity = false
    @State private var importPassword = ""
    @State private var statusMessage: String?

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                Image(systemName: "checkmark.seal")
                    .font(.system(size: 42))
                    .foregroundStyle(.secondary)

                Text("Signing Identity")
                    .font(.title2.bold())

                Text(hasIdentity
                     ? "A PKCS#12 identity is stored in this iPad's Keychain. It contains a certificate and its matching private key."
                     : "No signing identity is stored yet. Import an existing .p12 file below. The future Apple developer certificate flow can save its generated identity here.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)

                if hasIdentity {
                    Button {
                        exportIdentity()
                    } label: {
                        Label("Export Signing Identity (.p12)", systemImage: "square.and.arrow.up")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)

                    Button(role: .destructive) {
                        deleteIdentity()
                    } label: {
                        Label("Remove stored identity", systemImage: "trash")
                    }
                    .buttonStyle(.bordered)
                } else {
                    SecureField("Existing .p12 password", text: $importPassword)
                        .textFieldStyle(.roundedBorder)

                    Button {
                        isImporting = true
                    } label: {
                        Label("Import signing identity (.p12)", systemImage: "square.and.arrow.down")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                }

                if let statusMessage {
                    Text(statusMessage)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }

                Text("A .p12 contains a private signing key. Keep its password private. Importing or exporting an identity does not itself install or sign an IPA.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
            }
            .padding(24)
            .frame(maxWidth: 620)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle("Certificates")
        .fileImporter(
            isPresented: $isImporting,
            allowedContentTypes: [.pkcs12],
            allowsMultipleSelection: false
        ) { result in
            importIdentity(result)
        }
        .fileExporter(
            isPresented: $isExporting,
            document: exportDocument,
            contentType: .pkcs12,
            defaultFilename: "iLoader-Developer-Identity",
            onCompletion: { result in
                switch result {
                case .success:
                    statusMessage = "Signing identity exported successfully."
                    state.log(.info, "Exported PKCS#12 signing identity")
                case .failure(let error):
                    statusMessage = "Export failed: \(error.localizedDescription)"
                    state.log(.error, "PKCS#12 export failed: \(error.localizedDescription)")
                }
            }
        )
        .onAppear(perform: refreshIdentityStatus)
    }

    private func refreshIdentityStatus() {
        do {
            hasIdentity = try SigningIdentityStore.hasIdentity()
        } catch {
            hasIdentity = false
            statusMessage = "Could not check Keychain: \(error.localizedDescription)"
        }
    }

    private func importIdentity(_ result: Result<[URL], Error>) {
        do {
            guard let source = try result.get().first else { return }
            let didStartAccessing = source.startAccessingSecurityScopedResource()
            defer {
                if didStartAccessing {
                    source.stopAccessingSecurityScopedResource()
                }
            }
            let data = try Data(contentsOf: source)
            let metadata = try SigningIdentityStore.importPKCS12(data, password: importPassword)
            importPassword = ""
            hasIdentity = true
            statusMessage = "Imported identity: \(metadata.subject)"
            state.log(.info, "Imported PKCS#12 identity: \(metadata.subject)")
        } catch {
            statusMessage = error.localizedDescription
            state.log(.error, "PKCS#12 import failed: \(error.localizedDescription)")
        }
    }

    private func exportIdentity() {
        do {
            exportDocument = PKCS12Document(data: try SigningIdentityStore.exportData())
            isExporting = true
        } catch {
            hasIdentity = false
            statusMessage = error.localizedDescription
        }
    }

    private func deleteIdentity() {
        do {
            try SigningIdentityStore.deleteIdentity()
            hasIdentity = false
            statusMessage = "Stored signing identity removed from this iPad."
            state.log(.warning, "Removed stored PKCS#12 signing identity")
        } catch {
            statusMessage = "Could not remove identity: \(error.localizedDescription)"
        }
    }
}
