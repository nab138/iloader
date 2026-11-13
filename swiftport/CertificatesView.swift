import SwiftUI
import UniformTypeIdentifiers

/// PKCS#12 identity document. The .p12 must already contain the certificate
/// and matching private key, protected with a password when it was created.
struct PKCS12Document: FileDocument {
    static var readableContentTypes: [UTType] { [.pkcs12] }
    var data: Data

    init(data: Data) { self.data = data }

    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

struct CertificatesView: View {
    @EnvironmentObject private var state: AppState
    @State private var exportDocument = PKCS12Document(data: Data())
    @State private var isExporting = false
    @State private var hasIdentity = false
    @State private var statusMessage: String?

    private let keychainService = "app.iloader.signing"
    private let identityAccount = "developer-identity.p12"

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "checkmark.seal")
                .font(.system(size: 42))
                .foregroundStyle(.secondary)

            Text("Signing Certificate")
                .font(.title2.bold())

            Text(hasIdentity
                 ? "A signing identity is stored on this iPad. Export the .p12 file to use it with a compatible IPA-signing tool."
                 : "No signing identity is stored yet. After the Apple developer login and certificate service creates a PKCS#12 identity, it will be stored securely here.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)

            Button {
                exportIdentity()
            } label: {
                Label("Export Signing Certificate (.p12)", systemImage: "square.and.arrow.up")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(!hasIdentity)

            if let statusMessage {
                Text(statusMessage)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }

            Text("A .p12 contains a private signing key. Keep it private and use a strong export password when the identity is created. Do not share it in chats or upload it to public repositories.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.top, 8)
        }
        .padding()
        .navigationTitle("Certificates")
        .fileExporter(
            isPresented: $isExporting,
            document: exportDocument,
            contentType: .pkcs12,
            defaultFilename: "iLoader-Developer-Identity",
            onCompletion: { result in
                switch result {
                case .success:
                    statusMessage = "Signing identity exported."
                case .failure(let error):
                    statusMessage = "Export failed: \\(error.localizedDescription)"
                }
            }
        )
        .onAppear(perform: refreshIdentityStatus)
    }

    private func refreshIdentityStatus() {
        do {
            hasIdentity = try KeychainStore.read(
                service: keychainService,
                account: identityAccount
            ) != nil
        } catch {
            hasIdentity = false
            statusMessage = "Could not check Keychain: \\(error.localizedDescription)"
        }
    }

    private func exportIdentity() {
        do {
            guard let data = try KeychainStore.read(
                service: keychainService,
                account: identityAccount
            ), !data.isEmpty else {
                hasIdentity = false
                statusMessage = "No signing identity is available to export."
                return
            }
            exportDocument = PKCS12Document(data: data)
            isExporting = true
        } catch {
            statusMessage = "Could not read signing identity: \\(error.localizedDescription)"
        }
    }
}
