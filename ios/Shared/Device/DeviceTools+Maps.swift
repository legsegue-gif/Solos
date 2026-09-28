//  Place search, routes and travel times through MapKit.

import Foundation
import MapKit

extension DeviceTools {
    func maps(_ input: [String: Any]) throws -> [String: Any] {
        let action = input["action"] as? String ?? "search"
        switch action {
        case "search":
            guard let query = (input["query"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !query.isEmpty else { return ["error": "`query` is required."] }
            let lat = numberArg(input["lat"])
            let lon = numberArg(input["lon"])
            let radius = numberArg(input["radius"]) ?? 1000
            let limit = Int(numberArg(input["limit"]) ?? 10).clamped(to: 1...50)
            let request = MKLocalSearch.Request()
            request.naturalLanguageQuery = query
            // A centre narrows the search; without one MapKit searches
            // globally, which is the right answer for "where is the Eiffel
            // Tower" and the wrong one for "coffee". Optional rather than
            // required so the first kind of question does not need a
            // `device_location` call it has no use for.
            if let lat, let lon {
                request.region = MKCoordinateRegion(
                    center: CLLocationCoordinate2D(latitude: lat, longitude: lon),
                    latitudinalMeters: radius * 2,
                    longitudinalMeters: radius * 2
                )
            }
            // A region-constrained search fails on the simulator with
            // MKErrorPlacemarkNotFound while the same query without a region
            // succeeds — repeatedly, with and without location permission, and
            // for queries that plainly have results. Rather than hand that
            // back as "no coffee near you", fall back to searching without the
            // region and filter by distance here. The answer is the same shape
            // either way, and it is the right answer on a device where the
            // region search works too.
            var response: MKLocalSearch.Response
            var narrowed = true
            do {
                response = try awaitSearch(request)
            } catch {
                guard lat != nil, lon != nil, request.region.span.latitudeDelta > 0 else { throw error }
                let global = MKLocalSearch.Request()
                global.naturalLanguageQuery = query
                response = try awaitSearch(global)
                narrowed = false
            }
            // When the fallback ran, the radius has to be applied here instead
            // of by MapKit, and nearest-first is what "near me" meant.
            var candidates = response.mapItems
            if !narrowed, let lat, let lon {
                let here = CLLocation(latitude: lat, longitude: lon)
                candidates = candidates
                    .map { ($0, here.distance(from: CLLocation(latitude: $0.placemark.coordinate.latitude, longitude: $0.placemark.coordinate.longitude))) }
                    .filter { $0.1 <= radius }
                    .sorted { $0.1 < $1.1 }
                    .map { $0.0 }
            }
            let items = candidates.prefix(limit).map { item -> [String: Any] in
                var row: [String: Any] = ["name": item.name ?? ""]
                let c = item.placemark.coordinate
                row["lat"] = c.latitude
                row["lon"] = c.longitude
                if let address = formatted(item.placemark) { row["address"] = address }
                if let phone = item.phoneNumber { row["phone"] = phone }
                if let url = item.url?.absoluteString { row["url"] = url }
                // Distance from the centre the caller gave, because "which of
                // these is nearest" is the next question every time. Only
                // meaningful when there was a centre.
                if let lat, let lon {
                    let here = CLLocation(latitude: lat, longitude: lon)
                    row["distance_m"] = Int(here.distance(from: CLLocation(latitude: c.latitude, longitude: c.longitude)))
                }
                return row
            }
            return ["query": query, "items": Array(items), "radius_applied_locally": !narrowed]

        case "route", "eta":
            guard let from = input["from"] as? String, let to = input["to"] as? String else {
                return ["error": "`from` and `to` are required."]
            }
            let request = MKDirections.Request()
            request.source = try mapItem(from)
            request.destination = try mapItem(to)
            request.transportType = transport(input["mode"] as? String)
            let directions = MKDirections(request: request)
            if action == "eta" {
                // Much cheaper than a full route, and "how long does it take"
                // is what is being asked most of the time.
                let eta = try awaitETA(directions)
                return [
                    "from": from, "to": to,
                    "mode": input["mode"] as? String ?? "driving",
                    "duration_s": Int(eta.expectedTravelTime),
                    "duration_text": durationText(eta.expectedTravelTime),
                    "distance_m": Int(eta.distance),
                ]
            }
            let response = try awaitDirections(directions)
            guard let route = response.routes.first else { return ["error": "No route found."] }
            return [
                "from": from, "to": to,
                "mode": input["mode"] as? String ?? "driving",
                "duration_s": Int(route.expectedTravelTime),
                "duration_text": durationText(route.expectedTravelTime),
                "distance_m": Int(route.distance),
                "steps": route.steps.compactMap { step -> [String: Any]? in
                    guard !step.instructions.isEmpty else { return nil }
                    return ["instruction": step.instructions, "distance_m": Int(step.distance)]
                },
            ]

        default:
            return ["error": "Unknown action: \(action)."]
        }
    }

    func transport(_ mode: String?) -> MKDirectionsTransportType {
        switch mode {
        case "walking": return .walking
        case "transit": return .transit
        default: return .automobile
        }
    }

    /// `lat,lon` or an address. The coordinate form is tried first so a pair of
    /// numbers never goes to the geocoder and comes back as a street name.
    func mapItem(_ text: String) throws -> MKMapItem {
        let parts = text.split(separator: ",")
        if parts.count == 2, let lat = Double(parts[0].trimmingCharacters(in: .whitespaces)),
           let lon = Double(parts[1].trimmingCharacters(in: .whitespaces)) {
            let coord = CLLocationCoordinate2D(latitude: lat, longitude: lon)
            return MKMapItem(placemark: MKPlacemark(coordinate: coord))
        }
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = text
        guard let first = try awaitSearch(request).mapItems.first else {
            throw DeviceError.message("No place found for \(text).")
        }
        return first
    }

    func formatted(_ placemark: MKPlacemark) -> String? {
        let parts = [placemark.thoroughfare, placemark.locality, placemark.administrativeArea, placemark.country]
        let joined = parts.compactMap { $0 }.joined(separator: " ")
        return joined.isEmpty ? nil : joined
    }

    func durationText(_ seconds: TimeInterval) -> String {
        let total = Int(seconds.rounded())
        let h = total / 3600, m = (total % 3600) / 60
        if h > 0 { return "\(h) h \(m) min" }
        return "\(max(m, 1)) min"
    }

    /// MapKit is callback-based and every device tool here is synchronous (the
    /// core calls it on a blocking thread), so the wait happens here rather
    /// than turning the whole bridge async.
    func awaitSearch(_ request: MKLocalSearch.Request) throws -> MKLocalSearch.Response {
        let handoff = Handoff<MKLocalSearch.Response>()
        MKLocalSearch(request: request).start { response, error in
            handoff.finish(response.map { .success($0) } ?? .failure(error ?? DeviceError.message("The search failed.")))
        }
        guard let result = handoff.wait(seconds: 20) else {
            throw DeviceError.message("The map search timed out.")
        }
        return try result.get()
    }

    func awaitDirections(_ directions: MKDirections) throws -> MKDirections.Response {
        var result: Result<MKDirections.Response, Error>?
        let sem = DispatchSemaphore(value: 0)
        directions.calculate { response, error in
            result = response.map { .success($0) } ?? .failure(error ?? DeviceError.message("Routing failed."))
            sem.signal()
        }
        guard sem.wait(timeout: .now() + 30) == .success, let result else {
            throw DeviceError.message("Routing timed out.")
        }
        return try result.get()
    }

    func awaitETA(_ directions: MKDirections) throws -> MKDirections.ETAResponse {
        var result: Result<MKDirections.ETAResponse, Error>?
        let sem = DispatchSemaphore(value: 0)
        directions.calculateETA { response, error in
            result = response.map { .success($0) } ?? .failure(error ?? DeviceError.message("The travel time estimate failed."))
            sem.signal()
        }
        guard sem.wait(timeout: .now() + 20) == .success, let result else {
            throw DeviceError.message("The travel time estimate timed out.")
        }
        return try result.get()
    }
}
