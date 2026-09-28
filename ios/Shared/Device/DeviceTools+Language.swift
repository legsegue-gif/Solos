//  Language detection, tokens, entities and sentiment, through NaturalLanguage, on the device.

import Foundation
import NaturalLanguage

extension DeviceTools {
    func nlp(_ input: [String: Any]) throws -> [String: Any] {
        guard let text = input["text"] as? String, !text.isEmpty else {
            return ["error": "`text` is required."]
        }
        let action = input["action"] as? String ?? "analyze"
        var out: [String: Any] = [:]

        func language() -> [String: Any] {
            let recognizer = NLLanguageRecognizer()
            recognizer.processString(text)
            var row: [String: Any] = [:]
            if let dominant = recognizer.dominantLanguage { row["language"] = dominant.rawValue }
            row["candidates"] = recognizer.languageHypotheses(withMaximum: 3)
                .sorted { $0.value > $1.value }
                .map { ["language": $0.key.rawValue, "confidence": $0.value] }
            return row
        }
        func tokenize() -> [String] {
            let unit: NLTokenUnit
            switch input["unit"] as? String {
            case "sentence": unit = .sentence
            case "paragraph": unit = .paragraph
            default: unit = .word
            }
            let tokenizer = NLTokenizer(unit: unit)
            tokenizer.string = text
            return tokenizer.tokens(for: text.startIndex..<text.endIndex).map { String(text[$0]) }
        }
        func entities() -> [[String: Any]] {
            let tagger = NLTagger(tagSchemes: [.nameType])
            tagger.string = text
            var found: [[String: Any]] = []
            tagger.enumerateTags(
                in: text.startIndex..<text.endIndex,
                unit: .word,
                scheme: .nameType,
                options: [.omitPunctuation, .omitWhitespace, .joinNames]
            ) { tag, range in
                if let tag, [.personalName, .placeName, .organizationName].contains(tag) {
                    found.append(["text": String(text[range]), "kind": tag.rawValue])
                }
                return true
            }
            return found
        }
        /// The scheme constant and the scheme this OS actually offers are not
        /// always the same string: `NLTagScheme.sentimentScore` is
        /// "SentimentScore", while `availableTagSchemes` on iOS 27 reports
        /// "Sentiment". Asking for a scheme that is not on the list returns nil
        /// with no error, which reads exactly like "this language has no
        /// sentiment model" — so pick from what is offered instead of
        /// hard-coding the name.
        func sentimentScheme(for s: String) -> NLTagScheme? {
            let recognizer = NLLanguageRecognizer()
            recognizer.processString(s)
            guard let language = recognizer.dominantLanguage else { return nil }
            let available = NLTagger.availableTagSchemes(for: .paragraph, language: language)
            if available.contains(.sentimentScore) { return .sentimentScore }
            return available.first { $0.rawValue.localizedCaseInsensitiveContains("sentiment") }
        }
        /// Either a score or a label, because the two schemes answer
        /// differently: `SentimentScore` gives a number in -1…+1, while the
        /// newer `Sentiment` / `Emotion` schemes give a word. Reading the word
        /// as a number is how English came back as "no sentiment model" while
        /// Chinese worked.
        func sentiment(of s: String) -> (score: Double?, label: String?)? {
            guard let scheme = sentimentScheme(for: s) else { return nil }
            let tagger = NLTagger(tagSchemes: [scheme])
            tagger.string = s
            let (tag, _) = tagger.tag(at: s.startIndex, unit: .paragraph, scheme: scheme)
            guard let raw = tag?.rawValue else { return nil }
            if let score = Double(raw) { return (score, nil) }
            return (nil, raw)
        }
        func describe(_ result: (score: Double?, label: String?)) -> [String: Any] {
            var row: [String: Any] = [:]
            if let score = result.score {
                row["score"] = score
                row["scale"] = "-1 (negative) … +1 (positive)"
            }
            if let label = result.label { row["label"] = label }
            return row
        }

        switch action {
        case "language":
            out = language()
        case "tokenize":
            let tokens = tokenize()
            out["tokens"] = tokens
            out["count"] = tokens.count
        case "ner":
            let found = entities()
            out["entities"] = found
            if found.isEmpty {
                // Same reason as sentiment: an empty list can mean "no names
                // in this text" or "this device has no name model", and the
                // two call for completely different responses.
                let recognizer = NLLanguageRecognizer()
                recognizer.processString(text)
                let lang = recognizer.dominantLanguage
                out["detected_language"] = lang?.rawValue ?? "unknown"
                out["available_schemes"] = (lang.map { NLTagger.availableTagSchemes(for: .word, language: $0) } ?? [])
                    .map { $0.rawValue }
                // Not an error: no names in the text is a legitimate answer,
                // and the two fields above are what tells them apart.
                out["note"] = "An empty list means either that the text names no people, places or organisations, or that this device has no name model for the language; see whether available_schemes includes NameType."
            }
        case "sentiment":
            if input["per_sentence"] as? Bool == true {
                let tokenizer = NLTokenizer(unit: .sentence)
                tokenizer.string = text
                out["sentences"] = tokenizer.tokens(for: text.startIndex..<text.endIndex).map { range -> [String: Any] in
                    let sentence = String(text[range])
                    var row: [String: Any] = ["text": sentence]
                    if let result = sentiment(of: sentence) { row.merge(describe(result)) { a, _ in a } }
                    return row
                }
            } else if let result = sentiment(of: text) {
                // The scale is named rather than assumed: a bare number invites
                // the model to invent one.
                out.merge(describe(result)) { a, _ in a }
            } else {
                // Which it is matters: the sentiment model is an on-device
                // asset that is not present everywhere (notably not on a fresh
                // simulator), and "unsupported language" would send the caller
                // looking in the wrong place.
                let recognizer = NLLanguageRecognizer()
                recognizer.processString(text)
                let lang = recognizer.dominantLanguage
                let schemes = (lang.map { NLTagger.availableTagSchemes(for: .paragraph, language: $0) } ?? [])
                    .map { $0.rawValue }
                // Everything the caller needs goes in the message itself: a
                // result that carries `error` is rendered as an error envelope
                // and the sibling fields are dropped, so a diagnosis parked
                // next to it would never arrive.
                out["error"] = "No sentiment score (language detected: \(lang?.rawValue ?? "unknown"); "
                    + "schemes available for it: \(schemes.isEmpty ? "none" : schemes.joined(separator: ", "))). "
                    + "The sentiment model is an on-device resource that not every language or device has."
            }
        case "analyze":
            out["language"] = language()
            out["entities"] = entities()
            if let result = sentiment(of: text) { out["sentiment"] = describe(result) }
            out["word_count"] = tokenize().count
        default:
            return ["error": "Unknown action: \(action)."]
        }
        return out
    }
}
