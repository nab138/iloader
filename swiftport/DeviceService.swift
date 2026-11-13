import Foundation
import UIKit

/// Describes the iPad running this app. It intentionally does not expose
/// serial numbers, UDIDs, or other persistent hardware identifiers.
struct DeviceService {
    func currentDevice() -> CurrentDeviceInfo {
        let device = UIDevice.current
        return CurrentDeviceInfo(
            name: device.name,
            model: device.model,
            operatingSystem: "\(device.systemName) \(device.systemVersion)"
        )
    }
}
