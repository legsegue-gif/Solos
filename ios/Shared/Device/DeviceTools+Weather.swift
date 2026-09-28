//  Weather through WeatherKit, for the device's position or given coordinates.

import Foundation
import CoreLocation
import WeatherKit

extension DeviceTools {
    /// Shaped after the reference implementation's `apple-weather`:
    /// nothing specified means everything, for here. The field set follows
    /// its output too — the side-by-side test that prompted this showed the
    /// model saying "east wind, 11 km visibility, sunset 18:59" there and
    /// nothing of the kind here, because we never handed those over.
    func weather(_ input: [String: Any]) throws -> [String: Any] {
        let action = input["action"] as? String ?? "report"
        let valid = ["report", "current", "hourly", "daily", "minute", "alerts"]
        guard valid.contains(action) else {
            return ["error": "Unknown action \"\(action)\"; one of \(valid.joined(separator: ", "))."]
        }
        var out: [String: Any] = [:]
        let location: CLLocation
        switch (numberArg(input["lat"]), numberArg(input["lon"])) {
        case let (lat?, lon?):
            location = CLLocation(latitude: lat, longitude: lon)
        case (nil, nil):
            // The device's own location, as `apple-weather` does without
            // --lat/--lng. Saves the model a device_location round trip for
            // the commonest form of the question.
            location = try locationProvider.current()
            if let place = locationProvider.placemark(for: location) { out["place"] = place }
        default:
            return ["error": "Give both `lat` and `lon`, or neither (neither means where the device is)."]
        }
        out["lat"] = location.coordinate.latitude
        out["lon"] = location.coordinate.longitude
        let hours = Int(numberArg(input["hours"]) ?? 12).clamped(to: 1...48)
        let days = Int(numberArg(input["days"]) ?? 7).clamped(to: 1...10)

        let handoff = Handoff<[String: Any]>()
        // Only the task touches it from here on.
        nonisolated(unsafe) let initial = out
        Task {
            do {
                // One request for everything, as `apple-weather` does, never
                // `including:` a named dataset. Asking by name for one the
                // location does not have is, by every sign, a 400 for the
                // whole request: `report` (current + hourly + daily + alerts)
                // failed outright in Nanjing (2026-09-24, on device,
                // `responseFailed: 400`) while `current` alone had worked.
                // Without `including:`, WeatherKit checks what the
                // location supports first and leaves the rest nil.
                let weather = try await WeatherService.shared.weather(for: location)
                var out = initial
                // `report` is what `apple-weather report` is: minute-by-minute
                // stays out: sixty entries, and often unsupported anyway.
                let wantCurrent = action == "current" || action == "report"
                let wantHourly = action == "hourly" || action == "report"
                let wantDaily = action == "daily" || action == "report"
                let wantMinute = action == "minute"
                let wantAlerts = action == "alerts" || action == "report"

                if wantCurrent {
                    let now = weather.currentWeather
                    var current: [String: Any] = [
                        "condition": now.condition.description,
                        "temperature_c": now.temperature.converted(to: .celsius).value,
                        "feels_like_c": now.apparentTemperature.converted(to: .celsius).value,
                        "dew_point_c": now.dewPoint.converted(to: .celsius).value,
                        "humidity": now.humidity,
                        "cloud_cover": now.cloudCover,
                        "uv_index": now.uvIndex.value,
                        "visibility_km": now.visibility.converted(to: .kilometers).value,
                        "pressure_hpa": now.pressure.converted(to: .hectopascals).value,
                        "pressure_trend": now.pressureTrend.description,
                        "is_daylight": now.isDaylight,
                    ]
                    current.merge(Self.wind(now.wind)) { $1 }
                    out["current"] = current
                }
                if wantHourly {
                    let forecast = weather.hourlyForecast
                    // WeatherKit's hourly forecast starts well before now —
                    // `apple-weather report` at 15:51 began its list at 23:00
                    // the evening before — so taking the first N would hand
                    // over N hours of the past. Hours already gone are
                    // dropped; `hours: 12` means the next twelve.
                    let from = Date().addingTimeInterval(-3600)
                    out["hourly"] = forecast.forecast.filter { $0.date > from }.prefix(hours).map { h in
                        var hour: [String: Any] = [
                            "time": DateArg.format(h.date),
                            "condition": h.condition.description,
                            "temperature_c": h.temperature.converted(to: .celsius).value,
                            "feels_like_c": h.apparentTemperature.converted(to: .celsius).value,
                            "humidity": h.humidity,
                            "cloud_cover": h.cloudCover,
                            "uv_index": h.uvIndex.value,
                            "is_daylight": h.isDaylight,
                            // A probability, not an amount — the two get
                            // confused constantly, so both are reported and
                            // both are named.
                            "precip_chance": h.precipitationChance,
                            "precip_amount_mm": h.precipitationAmount.converted(to: .millimeters).value,
                        ]
                        hour.merge(Self.wind(h.wind)) { $1 }
                        return hour
                    }
                }
                if wantDaily {
                    let forecast = weather.dailyForecast
                    out["daily"] = forecast.forecast.prefix(days).map { d in
                        var day: [String: Any] = [
                            "date": DateArg.format(d.date),
                            "condition": d.condition.description,
                            "high_c": d.highTemperature.converted(to: .celsius).value,
                            "low_c": d.lowTemperature.converted(to: .celsius).value,
                            "precip_chance": d.precipitationChance,
                            "precip_amount_mm": d.precipitationAmount.converted(to: .millimeters).value,
                            "uv_index": d.uvIndex.value,
                        ]
                        day.merge(Self.wind(d.wind)) { $1 }
                        // Absent in polar day and night, which is a fact and
                        // not an error — left out rather than faked.
                        if let rise = d.sun.sunrise { day["sunrise"] = DateArg.format(rise) }
                        if let set = d.sun.sunset { day["sunset"] = DateArg.format(set) }
                        return day
                    }
                }
                if wantMinute {
                    // Absent is not the same as "no rain". Why it is absent
                    // is WeatherKit's to say, not ours: its own availability
                    // verdict for this location goes out alongside.
                    out["minute_availability"] = Self.availability(weather.availability.minuteAvailability)
                    let minute = weather.minuteForecast
                    if let minute {
                        out["minute"] = minute.forecast.prefix(60).map { m in
                            [
                                "time": DateArg.format(m.date),
                                "precip_chance": m.precipitationChance,
                                // Reported in whatever unit WeatherKit chose and
                                // named alongside, rather than converted: mm/h
                                // is not a member of UnitSpeed on every SDK,
                                // and a silently wrong unit is worse than a
                                // named one.
                                "precip_intensity": m.precipitationIntensity.value,
                                "precip_intensity_unit": m.precipitationIntensity.unit.symbol,
                            ]
                        }
                    } else {
                        out["minute"] = NSNull()
                    }
                }
                if wantAlerts {
                    // Same distinction as minute-by-minute: an empty list is
                    // "no alerts right now", a null one is not, and the
                    // availability verdict says which kind of null.
                    out["alerts_availability"] = Self.availability(weather.availability.alertAvailability)
                    if let alerts = weather.weatherAlerts {
                        out["alerts"] = alerts.map { a in
                            ["summary": a.summary, "severity": "\(a.severity)", "source": a.source]
                        }
                    } else {
                        out["alerts"] = NSNull()
                    }
                }
                handoff.finish(.success(out))
            } catch {
                handoff.finish(.failure(error))
            }
        }
        guard let result = handoff.wait(seconds: 30) else {
            return ["error": "The weather request timed out."]
        }
        return try result.get()
    }

    /// WeatherKit's verdict, passed through as a word the model can read.
    static func availability(_ kind: WeatherAvailability.AvailabilityKind) -> String {
        switch kind {
        case .available: return "available"
        case .temporarilyUnavailable: return "temporarily_unavailable"
        case .unsupported: return "unsupported"
        case .unknown: return "unknown"
        @unknown default: return "unknown"
        }
    }

    /// The same three wind fields for now, each hour and each day. The gust
    /// is left out when WeatherKit has none rather than reported as zero:
    /// "no gust data" and "no gusts" are different answers.
    static func wind(_ wind: Wind) -> [String: Any] {
        var out: [String: Any] = [
            "wind_speed_kmh": wind.speed.converted(to: .kilometersPerHour).value,
            "wind_direction": wind.compassDirection.description,
        ]
        if let gust = wind.gust { out["wind_gust_kmh"] = gust.converted(to: .kilometersPerHour).value }
        return out
    }
}
