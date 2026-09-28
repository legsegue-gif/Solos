//  Text, codes, labels and faces in an image, through Vision, on the device.

import Foundation
import CoreImage
import Vision

extension DeviceTools {
    func vision(_ input: [String: Any]) throws -> [String: Any] {
        guard let path = input["path"] as? String, let url = imageURL(path) else {
            return ["error": "No such image: \(input["path"] as? String ?? "(`path` is required)")."]
        }
        guard let image = CIImage(contentsOf: url) else {
            return ["error": "\(url.lastPathComponent) is not an image that can be read."]
        }
        let action = input["action"] as? String ?? "analyze"
        let handler = VNImageRequestHandler(ciImage: image, options: [:])
        var out: [String: Any] = ["path": path]

        func runOCR() throws -> [[String: Any]] {
            let request = VNRecognizeTextRequest()
            request.recognitionLevel = (input["level"] as? String) == "fast" ? .fast : .accurate
            if let langs = (input["lang"] as? String)?.split(separator: ",").map({
                $0.trimmingCharacters(in: .whitespaces)
            }), !langs.isEmpty {
                request.recognitionLanguages = langs
            }
            // Off by default in Vision; a screenshot of a form is exactly the
            // case where it helps.
            request.usesLanguageCorrection = true
            try handler.perform([request])
            return (request.results ?? []).compactMap { observation in
                guard let best = observation.topCandidates(1).first else { return nil }
                return ["text": best.string, "confidence": Double(best.confidence)]
            }
        }
        func runBarcode() throws -> [[String: Any]] {
            let request = VNDetectBarcodesRequest()
            try handler.perform([request])
            return (request.results ?? []).compactMap { r in
                guard let payload = r.payloadStringValue else { return nil }
                return ["payload": payload, "symbology": r.symbology.rawValue]
            }
        }
        func runClassify() throws -> [[String: Any]] {
            let request = VNClassifyImageRequest()
            try handler.perform([request])
            let limit = Int(numberArg(input["limit"]) ?? 5).clamped(to: 1...50)
            return (request.results ?? [])
                .filter { $0.confidence > 0.1 }
                .prefix(limit)
                .map { ["label": $0.identifier, "confidence": Double($0.confidence)] }
        }
        func runFaces() throws -> Int {
            let request = VNDetectFaceRectanglesRequest()
            try handler.perform([request])
            return request.results?.count ?? 0
        }

        switch action {
        case "ocr":
            let lines = try runOCR()
            out["lines"] = lines
            // The joined text is what a script wants to grep; the lines are
            // what a model wants to reason about. Both are cheap.
            out["text"] = lines.compactMap { $0["text"] as? String }.joined(separator: "\n")
        case "barcode":
            out["codes"] = try runBarcode()
        case "classify":
            out["labels"] = try runClassify()
        case "faces":
            out["faces"] = try runFaces()
        case "analyze":
            let lines = try runOCR()
            out["text"] = lines.compactMap { $0["text"] as? String }.joined(separator: "\n")
            out["labels"] = try runClassify()
            out["codes"] = try runBarcode()
        default:
            return ["error": "Unknown action: \(action)."]
        }
        return out
    }

    /// The core has already turned the model's path into the file on the
    /// device, refusing anything outside the workspace.
    func imageURL(_ path: String) -> URL? {
        FileManager.default.fileExists(atPath: path) ? URL(fileURLWithPath: path) : nil
    }
}
