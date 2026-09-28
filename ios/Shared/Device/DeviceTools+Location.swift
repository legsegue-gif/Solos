//  The device's position, one fix per call.

import Foundation
import CoreLocation

extension DeviceTools {
    func location() throws -> [String: Any] {
        let fix = try locationProvider.current()
        var out: [String: Any] = [
            "latitude": fix.coordinate.latitude,
            "longitude": fix.coordinate.longitude,
            "accuracy_m": fix.horizontalAccuracy,
            "timestamp": DateArg.format(fix.timestamp),
        ]
        if let place = locationProvider.placemark(for: fix) { out["place"] = place }
        return out
    }
}

/// One location fix on demand, without holding the hardware on between calls.
///
/// Asked from core threads, but the manager lives on main: a manager delivers
/// its callbacks on the run loop of the thread that made it, and a core
/// thread has none — made there, it never answers and every request times
/// out (as it did in the Probe, which opens the core off the main actor).
final class LocationProvider: NSObject, CLLocationManagerDelegate, @unchecked Sendable {
    // Both touched on main only.
    private var manager: CLLocationManager?
    private var waiters: [Handoff<CLLocation>] = []

    func current() throws -> CLLocation {
        let handoff = Handoff<CLLocation>()
        DispatchQueue.main.async {
            let manager = self.manager ?? self.makeManager()
            switch manager.authorizationStatus {
            case .denied, .restricted:
                handoff.finish(.failure(DeviceError.message("Access to location is denied; it can be allowed in Settings.")))
                return
            case .notDetermined:
                manager.requestWhenInUseAuthorization()
            default: break
            }
            self.waiters.append(handoff)
            manager.requestLocation()
        }
        guard let result = handoff.wait(seconds: 30) else {
            throw DeviceError.message("Getting the location timed out.")
        }
        return try result.get()
    }

    private func makeManager() -> CLLocationManager {
        let manager = CLLocationManager()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
        self.manager = manager
        return manager
    }

    private func answer(_ result: Result<CLLocation, Error>) {
        for waiter in waiters { waiter.finish(result) }
        waiters.removeAll()
    }

    func placemark(for location: CLLocation) -> String? {
        let semaphore = DispatchSemaphore(value: 0)
        var description: String?
        CLGeocoder().reverseGeocodeLocation(location) { places, _ in
            if let p = places?.first {
                description = [p.locality, p.subLocality, p.thoroughfare, p.name]
                    .compactMap { $0 }
                    .joined(separator: " ")
            }
            semaphore.signal()
        }
        _ = semaphore.wait(timeout: .now() + 10)
        return description
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let fix = locations.last else { return }
        answer(.success(fix))
    }

    func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        answer(.failure(Self.described(error)))
    }

    /// Core Location's own descriptions are the bare domain and code
    /// ("kCLErrorDomain error 0"); these say what the codes mean.
    private static func described(_ error: Error) -> Error {
        switch (error as? CLError)?.code {
        case .locationUnknown?: DeviceError.message("The device's location is not known right now.")
        case .denied?: DeviceError.message("Access to location is denied; it can be allowed in Settings.")
        case .network?: DeviceError.message("Finding the location needs the network, which is not available.")
        default: error
        }
    }
}
