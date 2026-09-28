//  The system music player: what is playing, search, play and pause.

import Foundation
import MediaPlayer

extension DeviceTools {
    /// The system player, not ours. Whatever this changes is already on the
    /// Lock Screen and in Control Centre, so it can be seen without a screen of
    /// our own.
    func media(_ input: [String: Any]) throws -> [String: Any] {
        let player = MPMusicPlayerController.systemMusicPlayer
        let action = input["action"] as? String ?? "now_playing"

        func nowPlaying() -> [String: Any] {
            var out: [String: Any] = [
                "state": Self.playbackName(player.playbackState),
            ]
            guard let item = player.nowPlayingItem else {
                out["item"] = NSNull()
                return out
            }
            out["item"] = [
                "title": item.title ?? "",
                "artist": item.artist ?? "",
                "album": item.albumTitle ?? "",
                "duration_s": Int(item.playbackDuration),
            ]
            out["position_s"] = Int(player.currentPlaybackTime)
            return out
        }

        switch action {
        case "now_playing": return nowPlaying()
        case "play": player.play(); return nowPlaying()
        case "pause": player.pause(); return nowPlaying()
        case "toggle":
            player.playbackState == .playing ? player.pause() : player.play()
            return nowPlaying()
        case "next": player.skipToNextItem(); return nowPlaying()
        case "previous": player.skipToPreviousItem(); return nowPlaying()

        case "volume":
            // Reading is free; setting goes through MPVolumeView's slider,
            // which is the only route Apple leaves open to an app.
            if let level = numberArg(input["level"]) {
                let clamped = Float(level.clamped(to: 0...1))
                DispatchQueue.main.sync {
                    let view = MPVolumeView(frame: .zero)
                    if let slider = view.subviews.compactMap({ $0 as? UISlider }).first {
                        slider.value = clamped
                    }
                }
                return ["ok": true, "volume": Double(clamped)]
            }
            return ["volume": Double(AVAudioSession.sharedInstance().outputVolume)]

        case "search", "play_search":
            guard let query = input["query"] as? String, !query.isEmpty else {
                return ["error": "`query` is required."]
            }
            try requireMediaAccess()
            let limit = Int(numberArg(input["limit"]) ?? 20).clamped(to: 1...100)
            let items = Self.mediaSearch(query: query, type: input["type"] as? String, limit: limit)
            if action == "search" {
                return ["query": query, "items": items.map(Self.describe)]
            }
            guard let first = items.first else { return ["error": "Nothing found for \"\(query)\"."] }
            let collection = MPMediaItemCollection(items: [first])
            player.setQueue(with: collection)
            player.play()
            return ["ok": true, "playing": Self.describe(first)]

        default:
            return ["error": "Unknown action: \(action)."]
        }
    }

    static func playbackName(_ state: MPMusicPlaybackState) -> String {
        switch state {
        case .playing: return "playing"
        case .paused: return "paused"
        case .stopped: return "stopped"
        case .interrupted: return "interrupted"
        case .seekingForward, .seekingBackward: return "seeking"
        @unknown default: return "unknown"
        }
    }

    static func mediaSearch(query: String, type: String?, limit: Int) -> [MPMediaItem] {
        let property: String
        switch type {
        case "album": property = MPMediaItemPropertyAlbumTitle
        case "artist": property = MPMediaItemPropertyArtist
        case "playlist": property = MPMediaPlaylistPropertyName
        default: property = MPMediaItemPropertyTitle
        }
        let mediaQuery = MPMediaQuery.songs()
        mediaQuery.addFilterPredicate(
            MPMediaPropertyPredicate(value: query, forProperty: property, comparisonType: .contains)
        )
        return Array((mediaQuery.items ?? []).prefix(limit))
    }

    static func describe(_ item: MPMediaItem) -> [String: Any] {
        [
            "id": String(item.persistentID),
            "title": item.title ?? "",
            "artist": item.artist ?? "",
            "album": item.albumTitle ?? "",
            "duration_s": Int(item.playbackDuration),
        ]
    }

    func requireMediaAccess() throws {
        let status = MPMediaLibrary.authorizationStatus()
        if status == .authorized { return }
        if status == .denied || status == .restricted {
            throw DeviceError.message("Not allowed to access the media library.")
        }
        let sem = DispatchSemaphore(value: 0)
        var granted = false
        MPMediaLibrary.requestAuthorization { state in
            granted = state == .authorized
            sem.signal()
        }
        guard sem.wait(timeout: .now() + Self.personDeadline) == .success, granted else {
            throw DeviceError.message("Not allowed to access the media library.")
        }
    }
}
