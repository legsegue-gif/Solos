import Foundation
import Markdown

/// A reply's Markdown as blocks the chat draws. Parsing is CommonMark with
/// GitHub's tables, task lists and strikethrough (swift-markdown, which is
/// cmark-gfm), plus TeX math, which CommonMark does not know: `$$…$$` and
/// `\[…\]` blocks, `$…$` and `\(…\)` inline. Pure: no UI, unit-tested.
enum MDBlock: Equatable {
    case heading(level: Int, MDInline)
    case paragraph(MDInline)
    case code(language: String?, String)
    case math(String)
    case quote([MDBlock])
    case list(ordered: Bool, start: Int, items: [MDListItem])
    case table(header: [MDInline], rows: [[MDInline]], alignments: [MDAlignment])
    case rule
    /// An image, video or audio link alone in its paragraph.
    case media(url: String, alt: String)
}

struct MDListItem: Equatable {
    /// `nil` for a plain item, else whether its task box is ticked.
    var checked: Bool?
    var blocks: [MDBlock]
}

enum MDAlignment: Equatable { case leading, center, trailing }

/// Inline content: styled text with math between it.
typealias MDInline = [MDRun]

enum MDRun: Equatable {
    case text(AttributedString)
    case math(String)
}

enum MarkdownModel {
    /// Marks inline math that was turned into a code span before parsing,
    /// so CommonMark leaves the TeX alone. A private-use character, which no
    /// model writes.
    static let mathMark: Character = "\u{E000}"

    static func parse(_ text: String) -> [MDBlock] {
        let doc = Document(parsing: protectMath(text), options: [.disableSmartOpts])
        return blocks(doc.children)
    }

    // MARK: - Math

    /// Rewrite TeX so CommonMark keeps it intact: display math becomes a
    /// fenced `math` block, inline math a code span starting with
    /// `mathMark`. Code blocks and code spans are left as they are.
    static func protectMath(_ text: String) -> String {
        var out: [String] = []
        var fence: String?
        var display: (close: String, lines: [String])?
        for line in text.components(separatedBy: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if let d = display {
                if let range = trimmed.range(of: d.close) {
                    let last = String(trimmed[..<range.lowerBound])
                    out.append(contentsOf: ["```math"] + d.lines + (last.isEmpty ? [] : [last]) + ["```"])
                    display = nil
                } else {
                    display = (d.close, d.lines + [line])
                }
                continue
            }
            if let f = fence {
                out.append(line)
                if trimmed.hasPrefix(f) { fence = nil }
                continue
            }
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                fence = String(trimmed.prefix(3))
                out.append(line)
                continue
            }
            if let (open, close) = [("$$", "$$"), ("\\[", "\\]")].first(where: { trimmed.hasPrefix($0.0) }) {
                let rest = String(trimmed.dropFirst(open.count))
                if let range = rest.range(of: close) {
                    // Opened and closed on one line.
                    let tex = String(rest[..<range.lowerBound]).trimmingCharacters(in: .whitespaces)
                    let after = String(rest[range.upperBound...]).trimmingCharacters(in: .whitespaces)
                    if after.isEmpty {
                        out.append(contentsOf: ["```math", tex, "```"])
                        continue
                    }
                } else {
                    display = (close, rest.isEmpty ? [] : [rest])
                    continue
                }
            }
            out.append(protectInlineMath(line))
        }
        if let d = display {
            // Still open (a reply being streamed): show what there is.
            out.append(contentsOf: ["```math"] + d.lines + ["```"])
        }
        return out.joined(separator: "\n")
    }

    /// `$x$` and `\(x\)` on one line become code spans, skipping existing
    /// code spans. `$` counts as math only as Pandoc reads it: the opening
    /// one not followed by a space, the closing one not preceded by a space
    /// nor followed by a digit, so "$5 and $10" stays money.
    static func protectInlineMath(_ line: String) -> String {
        let chars = Array(line)
        var out = ""
        var i = 0
        func span(_ tex: String) -> String {
            let ticks = tex.contains("`") ? "``" : "`"
            return "\(ticks)\(mathMark)\(tex)\(ticks)"
        }
        while i < chars.count {
            let c = chars[i]
            if c == "`" {
                // Copy a code span through unchanged.
                var j = i
                while j < chars.count, chars[j] == "`" { j += 1 }
                let run = j - i
                var k = j
                var closed: Int?
                while k < chars.count {
                    if chars[k] == "`" {
                        var m = k
                        while m < chars.count, chars[m] == "`" { m += 1 }
                        if m - k == run { closed = m; break }
                        k = m
                    } else { k += 1 }
                }
                let end = closed ?? j
                out += String(chars[i..<end])
                i = end
                continue
            }
            if c == "\\", i + 1 < chars.count, chars[i + 1] == "(" {
                if let close = find(["\\", ")"], in: chars, from: i + 2) {
                    out += span(String(chars[(i + 2)..<close]))
                    i = close + 2
                    continue
                }
            }
            if c == "\\", i + 1 < chars.count, chars[i + 1] == "$" {
                out += "\\$"
                i += 2
                continue
            }
            if c == "$", i + 1 < chars.count, chars[i + 1] != "$", !chars[i + 1].isWhitespace {
                var k = i + 1
                var close: Int?
                while k < chars.count {
                    if chars[k] == "\\" { k += 2; continue }
                    if chars[k] == "$" {
                        let before = chars[k - 1]
                        let after: Character? = k + 1 < chars.count ? chars[k + 1] : nil
                        if !before.isWhitespace, !(after?.isNumber ?? false) { close = k }
                        break
                    }
                    k += 1
                }
                if let close, close > i + 1 {
                    out += span(String(chars[(i + 1)..<close]))
                    i = close + 1
                    continue
                }
            }
            out.append(c)
            i += 1
        }
        return out
    }

    private static func find(_ pattern: [Character], in chars: [Character], from start: Int) -> Int? {
        guard chars.count >= pattern.count else { return nil }
        var i = start
        while i <= chars.count - pattern.count {
            if Array(chars[i..<(i + pattern.count)]) == pattern { return i }
            i += 1
        }
        return nil
    }

    // MARK: - Blocks

    private static func blocks(_ children: MarkupChildren) -> [MDBlock] {
        children.flatMap { m -> [MDBlock] in
            if let p = m as? Paragraph, let media = standaloneMedia(p) { return media }
            return block(m).map { [$0] } ?? []
        }
    }

    private static func block(_ m: Markup) -> MDBlock? {
        switch m {
        case let h as Heading:
            return .heading(level: h.level, inline(h.children))
        case let p as Paragraph:
            return .paragraph(inline(p.children))
        case let c as CodeBlock:
            var code = c.code
            if code.hasSuffix("\n") { code.removeLast() }
            if c.language?.lowercased() == "math" || c.language?.lowercased() == "latex" && looksLikeMath(code) {
                return .math(code)
            }
            return .code(language: c.language.flatMap { $0.isEmpty ? nil : $0 }, code)
        case let q as BlockQuote:
            return .quote(blocks(q.children))
        case let l as UnorderedList:
            return .list(ordered: false, start: 1, items: l.listItems.map(item))
        case let l as OrderedList:
            return .list(ordered: true, start: Int(l.startIndex), items: l.listItems.map(item))
        case let t as Table:
            let header = t.head.cells.map { inline($0.children) }
            let rows = t.body.rows.map { row in Array(row.cells.map { inline($0.children) }) }
            let alignments: [MDAlignment] = t.columnAlignments.map {
                switch $0 {
                case .center: .center
                case .right: .trailing
                default: .leading
                }
            }
            return .table(header: Array(header), rows: Array(rows), alignments: alignments)
        case is ThematicBreak:
            return .rule
        case let h as HTMLBlock:
            let raw = h.rawHTML.trimmingCharacters(in: .whitespacesAndNewlines)
            return raw.isEmpty ? nil : .paragraph([.text(AttributedString(raw))])
        default:
            let s = m.format().trimmingCharacters(in: .whitespacesAndNewlines)
            return s.isEmpty ? nil : .paragraph([.text(AttributedString(s))])
        }
    }

    /// A ```latex block is math only when it is not a whole document.
    private static func looksLikeMath(_ code: String) -> Bool {
        !code.contains("\\documentclass") && !code.contains("\\begin{document}")
    }

    private static func item(_ li: ListItem) -> MDListItem {
        let checked: Bool? = li.checkbox.map { $0 == .checked }
        return MDListItem(checked: checked, blocks: blocks(li.children))
    }

    /// A paragraph that holds only images — one, or several on their own
    /// lines, as models write a gallery — is those images, shown as media.
    private static func standaloneMedia(_ p: Paragraph) -> [MDBlock]? {
        let children = Array(p.children).filter {
            !($0 is SoftBreak) && !($0 is LineBreak)
                && !(($0 as? Markdown.Text)?.string.trimmingCharacters(in: .whitespaces).isEmpty ?? false)
        }
        guard !children.isEmpty else { return nil }
        var out: [MDBlock] = []
        for child in children {
            guard let image = child as? Markdown.Image, let src = image.source, !src.isEmpty else { return nil }
            out.append(.media(url: src, alt: image.plainText))
        }
        return out
    }

    // MARK: - Inline

    private struct Style {
        var intent: InlinePresentationIntent = []
        var link: URL?
    }

    static func inline(_ children: MarkupChildren) -> MDInline {
        var runs: MDInline = []
        func appendText(_ s: String, _ style: Style) {
            var a = AttributedString(s)
            if !style.intent.isEmpty { a.inlinePresentationIntent = style.intent }
            if let link = style.link { a.link = link }
            if case .text(let prev) = runs.last {
                runs[runs.count - 1] = .text(prev + a)
            } else {
                runs.append(.text(a))
            }
        }
        func walk(_ m: Markup, _ style: Style) {
            switch m {
            case let t as Markdown.Text:
                appendText(t.string, style)
            case let c as InlineCode:
                if c.code.first == mathMark {
                    runs.append(.math(String(c.code.dropFirst())))
                } else {
                    var s = style
                    s.intent.insert(.code)
                    appendText(c.code, s)
                }
            case is SoftBreak:
                appendText(" ", style)
            case is LineBreak:
                appendText("\n", style)
            case let e as Emphasis:
                var s = style
                s.intent.insert(.emphasized)
                for c in e.children { walk(c, s) }
            case let e as Strong:
                var s = style
                s.intent.insert(.stronglyEmphasized)
                for c in e.children { walk(c, s) }
            case let e as Strikethrough:
                var s = style
                s.intent.insert(.strikethrough)
                for c in e.children { walk(c, s) }
            case let l as Markdown.Link:
                var s = style
                s.link = l.destination.flatMap(linkURL)
                if l.childCount == 0 {
                    appendText(l.destination ?? "", s)
                } else {
                    for c in l.children { walk(c, s) }
                }
            case let img as Markdown.Image:
                // An image inside text: a link to it, named by its alt text.
                var s = style
                s.link = img.source.flatMap(linkURL)
                let alt = img.plainText
                appendText(alt.isEmpty ? (img.source ?? "") : alt, s)
            case let h as InlineHTML:
                appendText(h.rawHTML, style)
            default:
                for c in m.children { walk(c, style) }
            }
        }
        for c in children { walk(c, Style()) }
        return runs
    }

    /// The reply as plain text, for places that cannot draw Markdown (a
    /// notification): blocks one per line, math as its TeX.
    static func plainText(_ text: String) -> String {
        func lines(_ blocks: [MDBlock]) -> [String] {
            blocks.flatMap { block -> [String] in
                switch block {
                case .heading(_, let i), .paragraph(let i): [i.plain]
                case .code(_, let code): [code]
                case .math(let tex): [tex]
                case .quote(let inner): lines(inner)
                case .list(_, _, let items): items.flatMap { lines($0.blocks) }
                case .table(let header, let rows, _): ([header] + rows).map { $0.map(\.plain).joined(separator: " | ") }
                case .rule: []
                case .media(_, let alt): [alt]
                }
            }
        }
        return lines(parse(text)).filter { !$0.isEmpty }.joined(separator: "\n")
    }

    /// Link targets as URLs; a workspace file with spaces or CJK in its name
    /// arrives unescaped, which `URL(string:)` would refuse.
    static func linkURL(_ destination: String) -> URL? {
        URL(string: destination)
            ?? destination.addingPercentEncoding(withAllowedCharacters: .urlFragmentAllowed).flatMap(URL.init(string:))
    }
}

extension MDInline {
    /// The text without styling, math as its TeX.
    var plain: String {
        map { run in
            switch run {
            case .text(let a): String(a.characters)
            case .math(let tex): tex
            }
        }.joined()
    }
}
