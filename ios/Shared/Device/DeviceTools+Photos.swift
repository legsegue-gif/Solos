//  The photo library, read-only: listed, and chosen photos copied into the workspace.

import Foundation
import Photos
import UIKit

extension DeviceTools {
    /// Read-only: the library is listed, and chosen photos are copied into
    /// the session's own files so a script in the sandbox can work on them.
    /// Nothing is ever added to or removed from the library.
    func photos(_ input: [String: Any]) throws -> [String: Any] {
        try requirePhotoAccess()
        switch input["action"] as? String {
        case "export":
            guard let ids = input["ids"] as? [String], !ids.isEmpty else {
                return ["error": "`ids` is required."]
            }
            guard let dir = input["_dir"] as? String else {
                return ["error": "There is no folder to write to."]
            }
            return try export(ids: ids, into: URL(fileURLWithPath: dir))

        // Listing is the default: asking for photos without saying which
        // almost always means "what is there".
        default:
            let limit = min(max(input["limit"] as? Int ?? 20, 1), 200)
            let options = PHFetchOptions()
            options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
            if let since = try DateArg.optional(input, "since") {
                options.predicate = NSPredicate(format: "creationDate > %@", since as NSDate)
            }
            options.fetchLimit = limit
            let assets = PHAsset.fetchAssets(with: .image, options: options)
            var items: [[String: Any]] = []
            assets.enumerateObjects { asset, _, _ in
                items.append([
                    "id": asset.localIdentifier,
                    "taken": asset.creationDate.map(DateArg.format) ?? "",
                    "width": asset.pixelWidth,
                    "height": asset.pixelHeight,
                    "favorite": asset.isFavorite,
                ])
            }
            return ["items": items]
        }
    }

    func export(ids: [String], into dir: URL) throws -> [String: Any] {
        let assets = PHAsset.fetchAssets(withLocalIdentifiers: ids, options: nil)
        guard assets.count > 0 else { return ["error": "None of these ids is a photo in the library."] }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        let manager = PHImageManager.default()
        let options = PHImageRequestOptions()
        options.isNetworkAccessAllowed = true   // iCloud photos are worth waiting for.
        options.isSynchronous = true
        options.version = .current
        options.deliveryMode = .highQualityFormat

        var files: [String] = []
        var failures: [String] = []
        assets.enumerateObjects { asset, _, _ in
            var written: String?
            manager.requestImageDataAndOrientation(for: asset, options: options) { data, uti, _, _ in
                guard let data else { return }
                let ext = Self.fileExtension(for: uti) ?? "jpg"
                let name = Self.exportName(for: asset, ext: ext)
                let url = dir.appendingPathComponent(name)
                if (try? data.write(to: url)) != nil { written = name }
            }
            if let written {
                files.append(written)
            } else {
                failures.append(asset.localIdentifier)
            }
        }
        if files.isEmpty {
            return ["error": "No photo could be exported."]
        }
        var out: [String: Any] = [
            "files": files,
            "message": "Exported \(files.count) to the workspace.",
        ]
        if !failures.isEmpty { out["failed"] = failures }
        return out
    }

    /// A name the model can talk about and a script can glob: when it was
    /// taken, plus enough of the id to keep two photos from the same second
    /// apart.
    static func exportName(for asset: PHAsset, ext: String) -> String {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        let stamp = formatter.string(from: asset.creationDate ?? Date())
        let suffix = asset.localIdentifier.prefix(8).replacingOccurrences(of: "/", with: "")
        return "photo-\(stamp)-\(suffix).\(ext)"
    }

    static func fileExtension(for uti: String?) -> String? {
        switch uti {
        case "public.png": return "png"
        case "public.heic", "public.heif": return "heic"
        case "com.compuserve.gif": return "gif"
        case "public.jpeg": return "jpg"
        default: return nil
        }
    }

    func requirePhotoAccess() throws {
        let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        switch status {
        case .authorized, .limited: return
        case .denied, .restricted: throw DeviceError.message("Access to photos is denied; it can be allowed in Settings.")
        default: break
        }
        let semaphore = DispatchSemaphore(value: 0)
        var granted = false
        PHPhotoLibrary.requestAuthorization(for: .readWrite) { new in
            granted = new == .authorized || new == .limited
            semaphore.signal()
        }
        _ = semaphore.wait(timeout: .now() + Self.personDeadline)
        if !granted { throw DeviceError.message("The user did not allow access to photos.") }
    }
}
