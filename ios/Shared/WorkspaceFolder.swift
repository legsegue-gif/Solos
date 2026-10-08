import Foundation

/// Where `/solos/ws` lives on the device: the app's Documents folder, which
/// the Files app shows (On My iPhone ▸ Solos) and edits in place.
enum WorkspaceFolder {
    static var path: String {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].path
    }
}
