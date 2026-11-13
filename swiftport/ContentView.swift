import SwiftUI
struct ContentView: View {
 @EnvironmentObject private var state: AppState
 var body: some View {
  NavigationSplitView {
   List {
    ForEach(WorkspaceSection.allCases) { item in
     Button { state.selectedSection = item } label: {
      Label(item.rawValue, systemImage: item.symbol)
       .foregroundStyle(state.selectedSection == item ? Color.accentColor : Color.primary)
     }
     .listRowBackground(state.selectedSection == item ? Color.accentColor.opacity(0.12) : Color.clear)
    }
   }
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