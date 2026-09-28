import XCTest
@testable import Solos

final class InfoPlistTests: XCTestCase {
    /// A permission prompt shows its key name when a strings table has the
    /// key and no value for the language: every English prompt once read
    /// "NSHealthShareUsageDescription".
    func testEveryPermissionPromptHasItsTextNotItsKey() {
        let info = Bundle.main.infoDictionary ?? [:]
        let keys = info.keys.filter { $0.hasSuffix("UsageDescription") }
        XCTAssertFalse(keys.isEmpty)
        for key in keys {
            let shown = Bundle.main.localizedInfoDictionary?[key] as? String ?? info[key] as? String
            XCTAssertNotEqual(shown, key, key)
            XCTAssertFalse(shown?.isEmpty ?? true, key)
        }
    }
}
