import Foundation
import Security

struct SigningIdentityMetadata {
    let subject: String
}

enum SigningIdentityError: LocalizedError {
    case invalidPKCS12
    case missingIdentity
    case noStoredIdentity

    var errorDescription: String? {
        switch self {
        case .invalidPKCS12: return "The selected file is not a valid PKCS#12 identity, or its password is incorrect."
        case .missingIdentity: return "The PKCS#12 file does not contain a certificate and its matching private key."
        case .noStoredIdentity: return "There is no signing identity saved on this iPad."
        }
    }
}

/// Stores an encrypted PKCS#12 identity in the iPad's Keychain.
/// This supports importing/exporting an identity; it does not create Apple's
/// developer certificate or authenticate an Apple developer account.
enum SigningIdentityStore {
    static let keychainService = "app.iloader.signing"
    static let keychainAccount = "developer-identity.p12"

    @discardableResult
    static func importPKCS12(_ data: Data, password: String) throws -> SigningIdentityMetadata {
        var importedItems: CFArray?
        let options = [kSecImportExportPassphrase as String: password] as CFDictionary
        let status = SecPKCS12Import(data as CFData, options, &importedItems)
        guard status == errSecSuccess else {
            throw SigningIdentityError.invalidPKCS12
        }

        guard
            let items = importedItems as? [[String: Any]],
            let first = items.first,
            let identity = first[kSecImportItemIdentity as String] as? SecIdentity
        else {
            throw SigningIdentityError.missingIdentity
        }

        var certificate: SecCertificate?
        guard SecIdentityCopyCertificate(identity, &certificate) == errSecSuccess,
              let certificate else {
            throw SigningIdentityError.missingIdentity
        }

        let subject = (SecCertificateCopySubjectSummary(certificate) as String?) ?? "Apple signing identity"
        try KeychainStore.save(data, service: keychainService, account: keychainAccount)
        return SigningIdentityMetadata(subject: subject)
    }

    static func hasIdentity() throws -> Bool {
        try KeychainStore.read(service: keychainService, account: keychainAccount) != nil
    }

    static func exportData() throws -> Data {
        guard let data = try KeychainStore.read(service: keychainService, account: keychainAccount),
              !data.isEmpty else {
            throw SigningIdentityError.noStoredIdentity
        }
        return data
    }

    static func deleteIdentity() throws {
        try KeychainStore.delete(service: keychainService, account: keychainAccount)
    }
}
