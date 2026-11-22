import SwiftUI
@main struct ILoaderApp: App {
 @StateObject private var appState = AppState()
 var body: some Scene { WindowGroup { ContentView().environmentObject(appState).preferredColorScheme(.dark) } }
}