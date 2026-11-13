import SwiftUI
struct AccountView: View {
 @EnvironmentObject private var state: AppState
 var body: some View {
  GroupBox("Apple developer account") {
   VStack(alignment: .leading, spacing: 12) {
    Label("Account sign-in is not connected", systemImage: "person.crop.circle").font(.headline)
    Text("The desktop implementation uses a dedicated developer-account and Anisette backend. This iPadOS port does not collect Apple ID passwords or verification codes and does not claim to authenticate or create signing sessions.").font(.footnote).foregroundStyle(.secondary)
    Button("Why unavailable?") { state.reportUnavailable("Apple developer account", reason: "A supported authentication and signing service must be implemented before this can be enabled.") }.buttonStyle(.bordered)
   }.frame(maxWidth: .infinity, alignment: .leading)
  }
 }
}