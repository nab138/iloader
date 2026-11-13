import Foundation
enum AccountServiceError: LocalizedError {
 case backendNotImplemented
 var errorDescription: String? { "The desktop Apple developer-account and Anisette backend has not been replaced with an iPadOS-compatible service." }
}
struct AccountService {
 func currentAccount() async throws -> String? { throw AccountServiceError.backendNotImplemented }
 func signOut() async throws { throw AccountServiceError.backendNotImplemented }
}