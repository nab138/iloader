import SwiftUI
struct DevicesView: View {
 @EnvironmentObject private var state: AppState
 var body: some View {
  ContentUnavailableView { Label("No device bridge on iPadOS", systemImage: "ipad.and.iphone") }
  description: { Text("The desktop app discovers iPhones and iPads through usbmuxd and lockdown services. iPadOS apps cannot run that desktop USB stack or freely communicate with connected iOS devices.") }
  actions: { Button("Platform details") { state.reportUnavailable("USB device discovery", reason: "Use the macOS desktop build for device discovery, pairing handshakes, and installation operations.") }.buttonStyle(.borderedProminent) }
  .navigationTitle("Devices")
 }
}