import XCTest
@testable import Solos

final class MountsTests: XCTestCase {
    private let notes = Mount(name: "notes", path: "/var/notes", writable: false)
    private let work = Mount(name: "My Work", path: "/var/work", writable: true)

    func testAPathIsInASharedFolderByItsName() {
        let mounts = [notes, work]
        let hit = GuestPath.mount("/solos/mnt/My Work/a/b.txt", in: mounts)
        XCTAssertEqual(hit?.mount.name, "My Work")
        XCTAssertEqual(hit?.rest, "a/b.txt")
        XCTAssertEqual(GuestPath.mount("/solos/mnt/notes", in: mounts)?.rest, "")
        XCTAssertNil(GuestPath.mount("/solos/mnt/other/a", in: mounts))
        XCTAssertNil(GuestPath.mount("/solos/ws/a", in: mounts))
        XCTAssertNil(GuestPath.mount("/solos/mnt", in: mounts), "the root lists the folders; it is in none of them")
    }

    func testOnlyTheWorkspaceAndFoldersThatAllowChangesAreWritable() {
        let mounts = [notes, work]
        XCTAssertTrue(GuestPath.isWritable("/solos/ws/x", mounts: mounts))
        XCTAssertTrue(GuestPath.isWritable("/solos/mnt/My Work/x", mounts: mounts))
        XCTAssertFalse(GuestPath.isWritable("/solos/mnt/notes/x", mounts: mounts))
        XCTAssertFalse(GuestPath.isWritable("/solos/mnt", mounts: mounts))
        XCTAssertFalse(GuestPath.isWritable("/etc", mounts: mounts))
    }

    func testASharedFolderMapsToItsDeviceFolderAndTheRestToTheWorkspaceOrSystem() {
        let root = URL(fileURLWithPath: "/sys"), ws = URL(fileURLWithPath: "/ws")
        let mounts = [notes]
        XCTAssertEqual(GuestPath.device("/solos/mnt/notes/a/b", root: root, workspace: ws, mounts: mounts).path, "/var/notes/a/b")
        XCTAssertEqual(GuestPath.device("/solos/mnt/notes", root: root, workspace: ws, mounts: mounts).path, "/var/notes")
        XCTAssertEqual(GuestPath.device("/solos/ws/a", root: root, workspace: ws, mounts: mounts).path, "/ws/a")
        XCTAssertEqual(GuestPath.device("/etc/hosts", root: root, workspace: ws, mounts: mounts).path, "/sys/etc/hosts")
    }

    func testAFolderNameIsMadeUsableTheWayTheCoreWantsIt() {
        XCTAssertEqual(MountStore.usableName("Obsidian Vault"), "Obsidian Vault")
        XCTAssertEqual(MountStore.usableName("a/b:c%d"), "a-b-c-d")
        XCTAssertEqual(MountStore.usableName("..hidden"), "hidden")
        XCTAssertEqual(MountStore.usableName("///"), "---")
        XCTAssertEqual(MountStore.usableName("   "), "Folder")
        XCTAssertEqual(MountStore.usableName(String(repeating: "x", count: 100)).count, 64)
        for good in ["notes", "笔记", "My Notes", "a.b"] { XCTAssertTrue(MountStore.isUsable(good), good) }
        for bad in ["", ".x", "a/b", "a:b", " a", "a ", "50%", "q?", "a\nb"] { XCTAssertFalse(MountStore.isUsable(bad), bad) }
    }
}
