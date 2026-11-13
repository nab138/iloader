import Foundation
enum DeviceServiceError: LocalizedError {
 case unavailableOnIPadOS
 var errorDescription: String? { "USB device discovery is unavailable to this iPadOS app. The desktop implementation depends on usbmuxd and lockdown access." }
}
struct DeviceService {
 func discoverDevices() async throws -> [String] { throw DeviceServiceError.unavailableOnIPadOS }
}