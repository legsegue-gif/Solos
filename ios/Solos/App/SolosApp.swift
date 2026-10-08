import SwiftUI

#if DEBUG
/// Turning the simulator on screen without its window menu:
/// SOLOS_TEST_ORIENTATION = portrait | landscape asks the system for it, as a
/// person rotating the device would.
@MainActor
private func rotateForTesting() {
    guard let wanted = ProcessInfo.processInfo.environment["SOLOS_TEST_ORIENTATION"],
          let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { return }
    let mask: UIInterfaceOrientationMask = wanted == "landscape" ? .landscapeRight : .portrait
    NSLog("SOLOS_TEST_ORIENTATION \(wanted): scene orientation now \(scene.interfaceOrientation.rawValue)")
    scene.requestGeometryUpdate(.iOS(interfaceOrientations: mask)) { error in
        NSLog("SOLOS_TEST_ORIENTATION refused: \(error)")
    }
}
#endif

@main
struct SolosApp: App {
    @State private var app = AppCore()

    var body: some Scene {
        WindowGroup {
            SessionListView()
                .environment(app)
                .task { await app.start() }
                #if DEBUG
                .task {
                    try? await Task.sleep(for: .seconds(1))
                    rotateForTesting()
                }
                #endif
        }
    }
}
