import XCTest
@testable import Solos

final class FileBrowserTests: XCTestCase {
    /// Only the workspace is a folder of the app's own; everything else is
    /// the guest's filesystem, whose metadata a change from outside would
    /// not match.
    func testOnlyTheWorkspaceCanBeChanged() {
        XCTAssertTrue(GuestPath.isWritable("/solos/ws"))
        XCTAssertTrue(GuestPath.isWritable("/solos/ws/a/b.txt"))
        XCTAssertFalse(GuestPath.isWritable("/solos/wsx"))
        XCTAssertFalse(GuestPath.isWritable("/solos"))
        XCTAssertFalse(GuestPath.isWritable("/root/.ash_history"))
        XCTAssertFalse(GuestPath.isWritable("/"))
    }

    func testThePathBarWalksDownFromTheRoot() {
        XCTAssertEqual(GuestPath.crumbs("/").map(\.path), ["/"])
        XCTAssertEqual(GuestPath.crumbs("/solos/ws/out").map(\.name), ["/", "solos", "ws", "out"])
        XCTAssertEqual(GuestPath.crumbs("/solos/ws/out").map(\.path), ["/", "/solos", "/solos/ws", "/solos/ws/out"])
        XCTAssertEqual(GuestPath.parent("/solos/ws"), "/solos")
        XCTAssertEqual(GuestPath.parent("/etc"), "/")
        XCTAssertEqual(GuestPath.parent("/"), "/")
        XCTAssertEqual(GuestPath.child("/", "etc"), "/etc")
    }

    func testTheWorkspaceMapsToItsOwnFolderAndTheRestIntoTheGuestTree() {
        let root = URL(fileURLWithPath: "/data/rootfs")
        let ws = URL(fileURLWithPath: "/data/workspace")
        XCTAssertEqual(GuestPath.device("/solos/ws", root: root, workspace: ws).path, "/data/workspace")
        XCTAssertEqual(GuestPath.device("/solos/ws/a/b.txt", root: root, workspace: ws).path, "/data/workspace/a/b.txt")
        XCTAssertEqual(GuestPath.device("/etc/profile", root: root, workspace: ws).path, "/data/rootfs/etc/profile")
        XCTAssertEqual(GuestPath.device("/", root: root, workspace: ws).path, "/data/rootfs")
    }

    func testSortingKeepsFoldersFirstAndFollowsTheChosenOrder() {
        func entry(_ name: String, dir: Bool = false, size: Int64 = 0, day: Double = 0) -> FileEntry {
            FileEntry(url: URL(fileURLWithPath: "/x/\(name)"), name: name, guestPath: "/x/\(name)", isDirectory: dir,
                      size: size, modified: Date(timeIntervalSince1970: day * 86_400))
        }
        let all = [entry("b.txt", size: 5, day: 3), entry("a.txt", size: 9, day: 1), entry("zdir", dir: true), entry("c.png", size: 1, day: 2)]
        let byName = all.sorted(by: FileSort.order(.name, ascending: true, foldersFirst: true)).map(\.name)
        XCTAssertEqual(byName, ["zdir", "a.txt", "b.txt", "c.png"])
        let newest = all.sorted(by: FileSort.order(.modified, ascending: false, foldersFirst: false)).map(\.name)
        XCTAssertEqual(newest.prefix(3), ["b.txt", "c.png", "a.txt"])
        let bySize = all.sorted(by: FileSort.order(.size, ascending: true, foldersFirst: true)).map(\.name)
        XCTAssertEqual(bySize, ["zdir", "c.png", "b.txt", "a.txt"])
    }
}
