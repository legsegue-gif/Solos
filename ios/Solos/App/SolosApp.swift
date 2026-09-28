import SwiftUI

@main
struct SolosApp: App {
    @State private var app = AppCore()

    var body: some Scene {
        WindowGroup {
            SessionListView()
                .environment(app)
                .task { await app.start() }
        }
    }
}
