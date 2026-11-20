import SwiftUI
struct AppIDsView: View {
 @EnvironmentObject private var state: AppState
 var body: some View {
  ContentUnavailableView { Label("App IDs need a signing service", systemImage: "app.badge") }
  description: { Text("App ID listing and deletion are developer portal actions in the original desktop backend. This port does not invent results or attempt unsupported requests.") }
  actions: { Button("Platform details") { state.reportUnavailable("App ID management", reason: "A supported authenticated developer portal implementation is required.") } }
  .navigationTitle("App IDs")
 }
}