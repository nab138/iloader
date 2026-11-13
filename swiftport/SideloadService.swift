import Foundation
enum SideloadServiceError: LocalizedError {
 case installationNotSupported
 var errorDescription: String? { "An ordinary iPadOS app cannot install arbitrary IPA files itself. Importing an IPA only stores the document in this app's sandbox." }
}
struct SideloadService {
 func installIPA(at url: URL) async throws { throw SideloadServiceError.installationNotSupported }
}