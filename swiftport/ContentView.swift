import SwiftUI
struct ContentView: View {
 @EnvironmentObject private var state: AppState
 var body: some View {
  NavigationSplitView {
   List(WorkspaceSection.allCases, selection: $state.selectedSection) { item in Label(item.rawValue, systemImage: item.symbol).tag(item) }
    .navigationTitle("iLoader").listStyle(.sidebar)
  } detail: {
   Group {
    switch state.selectedSection {
    case .home: DashboardView()
    case .devices: DevicesView()
    case .pairing: PairingView()
    case .certificates: CertificatesView()
    case .appIDs: AppIDsView()
    case .settings: SettingsView()
    }
   }.frame(maxWidth: .infinity, maxHeight: .infinity).background(Color(uiColor: .systemGroupedBackground))
  }
  .alert("Feature unavailable", isPresented: $state.showingAlert) { Button("OK", role: .cancel) {} } message: { Text(state.alertMessage) }
 }
}