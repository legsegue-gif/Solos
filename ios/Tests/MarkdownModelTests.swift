import XCTest
@testable import Solos

final class MarkdownModelTests: XCTestCase {
    private func plain(_ block: MDBlock) -> String? {
        switch block {
        case .paragraph(let i), .heading(_, let i): i.plain
        default: nil
        }
    }

    func testDisplayMathBecomesAMathBlockInEitherNotation() {
        XCTAssertEqual(MarkdownModel.parse("$$\nE = mc^2\n$$"), [.math("E = mc^2")])
        XCTAssertEqual(MarkdownModel.parse("\\[ a^2 + b^2 \\]"), [.math("a^2 + b^2")])
        XCTAssertEqual(MarkdownModel.parse("$$x_1 * y_1$$"), [.math("x_1 * y_1")])
    }

    func testAnUnclosedDisplayBlockWhileStreamingStillShowsAsMath() {
        XCTAssertEqual(MarkdownModel.parse("$$\n\\frac{1}{2"), [.math("\\frac{1}{2")])
    }

    func testInlineMathKeepsItsTeXAwayFromEmphasis() {
        guard case .paragraph(let runs) = MarkdownModel.parse("area $a*b*c$ and \\(x_1\\) done").first else {
            return XCTFail()
        }
        XCTAssertEqual(runs.compactMap { if case .math(let t) = $0 { t } else { nil } }, ["a*b*c", "x_1"])
    }

    func testMoneyIsNotMath() {
        let blocks = MarkdownModel.parse("It costs $5 and $10, or $ 3$.")
        XCTAssertEqual(blocks.compactMap(plain), ["It costs $5 and $10, or $ 3$."])
    }

    func testCodeIsLeftAlone() {
        XCTAssertEqual(MarkdownModel.parse("```sh\necho $HOME $PATH\n```"), [.code(language: "sh", "echo $HOME $PATH")])
        guard case .paragraph(let runs) = MarkdownModel.parse("run `echo $a$` now").first else { return XCTFail() }
        XCTAssertFalse(runs.contains { if case .math = $0 { true } else { false } })
        XCTAssertEqual(runs.plain, "run echo $a$ now")
    }

    func testHeadingsListsTasksAndQuotes() {
        let blocks = MarkdownModel.parse("## Results\n\n- [x] done\n- [ ] todo\n\n3. third\n\n> note")
        XCTAssertEqual(plain(blocks[0]), "Results")
        guard case .list(false, _, let tasks) = blocks[1], case .list(true, 3, _) = blocks[2], case .quote = blocks[3] else {
            return XCTFail("\(blocks)")
        }
        XCTAssertEqual(tasks.map(\.checked), [true, false])
    }

    func testTablesKeepTheirCellsAndAlignment() {
        guard case .table(let header, let rows, let align) = MarkdownModel.parse("| a | b |\n|:-|-:|\n| 1 | **2** |").first else {
            return XCTFail()
        }
        XCTAssertEqual(header.map(\.plain), ["a", "b"])
        XCTAssertEqual(rows.map { $0.map(\.plain) }, [["1", "2"]])
        XCTAssertEqual(align, [.leading, .trailing])
    }

    func testAnImageAloneIsMediaAndInsideTextIsALink() {
        XCTAssertEqual(MarkdownModel.parse("![chart](solos://ws/out/chart.png)"), [.media(url: "solos://ws/out/chart.png", alt: "chart")])
        guard case .paragraph(let runs) = MarkdownModel.parse("see ![chart](solos://ws/c.png) here").first,
              case .text(let a) = runs.first else { return XCTFail() }
        XCTAssertEqual(String(a.characters), "see chart here")
        XCTAssertTrue(a.runs.contains { $0.link?.absoluteString == "solos://ws/c.png" })
    }

    func testLinksToFilesWithUnescapedNamesStillWork() {
        guard case .paragraph(let runs) = MarkdownModel.parse("[报告](<solos://ws/报告 v2.md>)").first,
              case .text(let a) = runs.first else { return XCTFail() }
        XCTAssertNotNil(a.runs.first?.link)
    }

    func testPlainTextDropsTheMarkup() {
        XCTAssertEqual(MarkdownModel.plainText("Done:\n\n```\nok\n```\n\n**bold** and `code`"), "Done:\nok\nbold and code")
    }

    func testImagesOnConsecutiveLinesAreEachMedia() {
        XCTAssertEqual(MarkdownModel.parse("![a](solos://ws/a.png)\n![b](solos://ws/b.jpg)"),
                       [.media(url: "solos://ws/a.png", alt: "a"), .media(url: "solos://ws/b.jpg", alt: "b")])
    }
}
