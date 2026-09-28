//  BrowserDownloads.swift — files a page hands over.
//
//  A download is the one thing a web view produces that is not a page: no
//  navigation, no DOM change, nothing the model can see by looking. Before
//  this, tapping a download link did nothing at all — WebKit asks for a
//  delegate and, finding none, cancels.
//
//  Files land in the workspace (`/solos/ws/downloads`), so the moment one
//  finishes it is an ordinary file the model can read, unzip or parse with
//  the tools it already has. Nothing travels through the protocol: the bytes
//  go straight to disk on the path both halves already agree on.

import Foundation
import WebKit

@MainActor
final class DownloadCenter: NSObject, ObservableObject {
    static let shared = DownloadCenter()

    enum State: String {
        case running, finished, failed, cancelled
    }

    struct Item: Identifiable {
        let id: String
        var name: String
        var source: String
        var state: State
        var bytes: Int64 = 0
        var total: Int64 = 0
        var destination: URL?
        var error: String?

        var solosURL: String? {
            guard let destination else { return nil }
            return "solos://ws/downloads/\(destination.lastPathComponent)"
        }
    }

    @Published private(set) var items: [Item] = []

    /// State changes nobody has been told about yet. Drained by whoever asks
    /// first — the tool reports each download once, on the next action.
    private var notices: [String] = []
    private var counter = 0
    private var downloads: [String: WKDownload] = [:]

    /// The device folder behind `/solos/ws`, set when the core opens.
    var workspace: URL?

    /// `/solos/ws/downloads`, so the model reaches a download with its
    /// file tools and the user with a `solos://ws/downloads/…` link.
    private var directory: URL {
        (workspace ?? FileManager.default.temporaryDirectory).appendingPathComponent("downloads")
    }

    /// Take over a download WebKit is offering. Called from the navigation
    /// delegate, which is the only moment the hand-off is on the table.
    func adopt(_ download: WKDownload) {
        counter += 1
        let id = "dl\(counter)"
        let source = download.originalRequest?.url?.absoluteString ?? ""
        items.append(Item(id: id, name: URL(string: source)?.lastPathComponent ?? "download", source: source, state: .running))
        downloads[id] = download
        download.delegate = self
        notices.append(id)
    }

    func cancel(_ id: String) {
        guard let download = downloads[id] else { return }
        download.cancel { _ in }
        update(id) { $0.state = .cancelled }
    }

    /// Everything, newest last, with live byte counts read off the Progress
    /// objects rather than mirrored into our own state.
    func rows() -> [[String: Any]] {
        items.map { item in
            var row: [String: Any] = [
                "id": item.id,
                "name": item.name,
                "state": item.state.rawValue,
                "source": item.source,
            ]
            if let download = downloads[item.id], item.state == .running {
                row["bytes"] = download.progress.completedUnitCount
                row["total"] = max(download.progress.totalUnitCount, 0)
            } else {
                row["bytes"] = item.bytes
                row["total"] = item.total
            }
            if let url = item.solosURL { row["solos_url"] = url }
            if let error = item.error { row["error"] = error }
            return row
        }
    }

    /// The rows whose state the model has not heard about yet. Each download
    /// is announced when it starts and again when it settles, and never twice.
    func drainNotices() -> [[String: Any]] {
        guard !notices.isEmpty else { return [] }
        let wanted = Set(notices)
        notices.removeAll()
        return rows().filter { wanted.contains($0["id"] as? String ?? "") }
    }

    var runningCount: Int { items.filter { $0.state == .running }.count }

    private func update(_ id: String, _ change: (inout Item) -> Void) {
        guard let index = items.firstIndex(where: { $0.id == id }) else { return }
        change(&items[index])
        notices.append(id)
    }

    private func id(of download: WKDownload) -> String? {
        downloads.first { $0.value === download }?.key
    }

    /// A name nothing else in the directory already has. Overwriting the
    /// user's last export because a site reuses `download.pdf` is worse than
    /// a suffix nobody reads.
    private func vacant(for suggested: String) -> URL {
        let safe = suggested.isEmpty ? "download" : suggested.replacingOccurrences(of: "/", with: "_")
        var candidate = directory.appendingPathComponent(safe)
        guard FileManager.default.fileExists(atPath: candidate.path) else { return candidate }
        let ext = candidate.pathExtension
        let stem = candidate.deletingPathExtension().lastPathComponent
        var n = 2
        repeat {
            let name = ext.isEmpty ? "\(stem)-\(n)" : "\(stem)-\(n).\(ext)"
            candidate = directory.appendingPathComponent(name)
            n += 1
        } while FileManager.default.fileExists(atPath: candidate.path)
        return candidate
    }
}

extension DownloadCenter: WKDownloadDelegate {
    func download(
        _ download: WKDownload,
        decideDestinationUsing response: URLResponse,
        suggestedFilename: String,
        completionHandler: @escaping @MainActor @Sendable (URL?) -> Void
    ) {
        // The directory is created here rather than at launch: the guest
        // sees it through the same bind mount as the rest of the
        // workspace, and fakefs fills in its metadata on first access.
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let destination = vacant(for: suggestedFilename)
        if let id = id(of: download) {
            update(id) {
                $0.name = destination.lastPathComponent
                $0.destination = destination
                $0.total = max(response.expectedContentLength, 0)
            }
            // Starting is news; the name is only known now.
            notices.append(id)
        }
        completionHandler(destination)
    }

    func downloadDidFinish(_ download: WKDownload) {
                guard let id = id(of: download) else { return }
        let size = (try? FileManager.default.attributesOfItem(atPath: items.first { $0.id == id }?.destination?.path ?? ""))?[.size] as? Int64
        update(id) {
            $0.state = .finished
            $0.bytes = size ?? $0.bytes
        }
        downloads[id] = nil
    }

    func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
                guard let id = id(of: download) else { return }
        update(id) {
            // A cancel arrives here too; it is not a failure.
            if $0.state != .cancelled {
                $0.state = .failed
                $0.error = error.localizedDescription
            }
        }
        downloads[id] = nil
    }
}
