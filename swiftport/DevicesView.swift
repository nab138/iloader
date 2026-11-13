import SwiftUI

struct DevicesView: View {
    private let device = DeviceService().currentDevice()

    var body: some View {
        List {
            Section("This device") {
                Label {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(device.name).font(.headline)
                        Text("\(device.model) • \(device.operatingSystem)")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        Text("Current iPad running iLoader")
                            .font(.caption)
                            .foregroundStyle(.green)
                    }
                } icon: {
                    Image(systemName: "ipad")
                        .font(.title2)
                }
                .padding(.vertical, 6)
            }

            Section("Connected devices") {
                ContentUnavailableView(
                    "External device bridge unavailable",
                    systemImage: "cable.connector",
                    description: Text("This iPad app can identify itself, but iPadOS does not expose the desktop usbmuxd/lockdown USB device-management interface to ordinary apps. Use the supported desktop companion for managing a separate connected iPhone or iPad.")
                )
            }

            Section {
                Text("For privacy, iLoader displays the device name, model family, and OS version only. It does not collect the serial number or UDID.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Devices")
    }
}
