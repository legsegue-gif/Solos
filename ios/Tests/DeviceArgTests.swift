import XCTest
@testable import Solos

final class DeviceArgTests: XCTestCase {
    /// A time the model gave but the parser cannot read must come back as an
    /// error, not be dropped: a dropped `due` saved a reminder with no time.
    func testAnUnreadableTimeIsAnErrorAndAMissingOneIsNot() throws {
        XCTAssertThrowsError(try DateArg.optional(["due": "后天 10:00"], "due"))
        XCTAssertNil(try DateArg.optional([:], "due"))
        XCTAssertNil(try DateArg.optional(["due": " "], "due"))
        XCTAssertNil(try DateArg.optional(["due": NSNull()], "due"))
        let date = try XCTUnwrap(try DateArg.optional(["due": "tomorrow 09:00"], "due"))
        let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
        XCTAssertEqual([parts.hour, parts.minute], [9, 0])
    }
}
