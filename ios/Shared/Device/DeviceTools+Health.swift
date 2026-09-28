//  Health data through HealthKit.

import Foundation
import HealthKit

extension DeviceTools {
    /// The types worth naming, with the unit each one expects. HealthKit has
    /// hundreds; these are the ones a person actually asks about, and
    /// `list_types` is how the model finds the identifier rather than guessing
    /// at `HKQuantityTypeIdentifierStepCount` and getting it subtly wrong.
    static let healthTypes: [(id: String, desc: String, unit: String)] = [
        ("stepCount", "steps", "count"),
        ("distanceWalkingRunning", "walking and running distance", "m"),
        ("flightsClimbed", "flights climbed", "count"),
        ("activeEnergyBurned", "active energy", "kcal"),
        ("basalEnergyBurned", "resting energy", "kcal"),
        ("heartRate", "heart rate", "count/min"),
        ("restingHeartRate", "resting heart rate", "count/min"),
        ("heartRateVariabilitySDNN", "heart rate variability (SDNN)", "ms"),
        ("oxygenSaturation", "blood oxygen", "%"),
        ("respiratoryRate", "respiratory rate", "count/min"),
        ("bodyMass", "body weight", "kg"),
        ("height", "height", "m"),
        ("bodyFatPercentage", "body fat percentage", "%"),
        ("bloodGlucose", "blood glucose", "mg/dL"),
        ("dietaryWater", "water", "ml"),
        ("dietaryEnergyConsumed", "dietary energy", "kcal"),
        ("vo2Max", "VO2 max", "ml/(kg*min)"),
        ("sleepAnalysis", "sleep analysis (stages, not a number)", "—"),
    ]

    func health(_ input: [String: Any]) throws -> [String: Any] {
        guard HKHealthStore.isHealthDataAvailable() else {
            return ["error": "This device has no Health data."]
        }
        let action = input["action"] as? String ?? "list_types"
        if action == "list_types" {
            return ["types": Self.healthTypes.map { ["id": $0.id, "desc": $0.desc, "unit": $0.unit] }]
        }

        let store = healthStore
        switch action {
        case "read":
            let ids = (input["types"] as? [String]) ?? []
            guard !ids.isEmpty else { return ["error": "`types` is required; list_types gives the identifiers."] }
            let from = try DateArg.optional(input, "from") ?? Calendar.current.date(byAdding: .day, value: -7, to: Date())!
            let to = try DateArg.optional(input, "to") ?? Date()
            try requireHealthRead(ids, store: store)
            var out: [String: Any] = ["from": DateArg.format(from), "to": DateArg.format(to)]
            var series: [String: Any] = [:]
            for id in ids {
                series[id] = try readHealth(id, from: from, to: to, bucket: input["bucket"] as? String, store: store)
            }
            out["series"] = series
            return out

        case "log":
            guard let id = (input["types"] as? [String])?.first ?? input["type"] as? String else {
                return ["error": "`type` to write is required."]
            }
            guard let value = numberArg(input["value"]) else { return ["error": "`value` is required."] }
            guard let quantityType = HKQuantityType.quantityType(forIdentifier: HKQuantityTypeIdentifier(rawValue: "HKQuantityTypeIdentifier" + id.prefix(1).uppercased() + id.dropFirst())) else {
                return ["error": "\(id) is not a numeric type that can be written."]
            }
            let unitText = input["unit"] as? String ?? Self.healthTypes.first { $0.id == id }?.unit ?? "count"
            try requireHealthWrite(quantityType, store: store)
            let at = try DateArg.optional(input, "at") ?? Date()
            let sample = HKQuantitySample(
                type: quantityType,
                quantity: HKQuantity(unit: HKUnit(from: unitText), doubleValue: value),
                start: at,
                end: at
            )
            try awaitHealthSave(sample, store: store)
            return ["ok": true, "id": sample.uuid.uuidString, "type": id, "value": value, "unit": unitText]

        case "delete":
            guard let idText = input["id"] as? String, let uuid = UUID(uuidString: idText) else {
                return ["error": "`id` to delete is required."]
            }
            // Only samples this app wrote; HealthKit refuses the rest anyway,
            // and saying so up front is better than a framework error.
            return try deleteHealthSample(uuid, store: store)

        default:
            return ["error": "Unknown action: \(action)."]
        }
    }

    /// HealthKit identifiers are `HKQuantityTypeIdentifierStepCount` in full;
    /// the model is given the short form, so this puts the prefix back.
    func healthSampleType(_ id: String) -> HKSampleType? {
        let camel = id.prefix(1).uppercased() + id.dropFirst()
        if id == "sleepAnalysis" {
            return HKCategoryType.categoryType(forIdentifier: .sleepAnalysis)
        }
        return HKQuantityType.quantityType(forIdentifier: HKQuantityTypeIdentifier(rawValue: "HKQuantityTypeIdentifier" + camel))
    }

    /// HealthKit never says "the user said no" for reads — an unauthorised
    /// read returns an empty result, indistinguishable from "no data". So the
    /// prompt is requested up front and an empty answer afterwards can be
    /// reported as genuinely empty.
    func requireHealthRead(_ ids: [String], store: HKHealthStore) throws {
        let types = Set(ids.compactMap { healthSampleType($0) })
        guard !types.isEmpty else { throw DeviceError.message("None of these types is known; list_types gives the identifiers.") }
        var failure: Error?
        let sem = DispatchSemaphore(value: 0)
        store.requestAuthorization(toShare: [], read: types) { _, error in
            failure = error
            sem.signal()
        }
        guard sem.wait(timeout: .now() + Self.personDeadline) == .success else {
            throw DeviceError.message("Waiting for Health authorisation timed out.")
        }
        if let failure { throw failure }
    }

    func requireHealthWrite(_ type: HKSampleType, store: HKHealthStore) throws {
        var failure: Error?
        let sem = DispatchSemaphore(value: 0)
        store.requestAuthorization(toShare: [type], read: []) { _, error in
            failure = error
            sem.signal()
        }
        guard sem.wait(timeout: .now() + Self.personDeadline) == .success else {
            throw DeviceError.message("Waiting for Health write authorisation timed out.")
        }
        if let failure { throw failure }
        guard store.authorizationStatus(for: type) == .sharingAuthorized else {
            throw DeviceError.message("Not allowed to write this Health data.")
        }
    }

    func readHealth(
        _ id: String, from: Date, to: Date, bucket: String?, store: HKHealthStore
    ) throws -> Any {
        guard let type = healthSampleType(id) else { return ["error": "Unknown type: \(id)."] }
        let predicate = HKQuery.predicateForSamples(withStart: from, end: to)

        if let quantityType = type as? HKQuantityType, bucket != "none" {
            // Cumulative things (steps, energy) are summed per day; everything
            // else is averaged. Asking for "a week of steps" and getting 4,000
            // raw samples is not an answer.
            let cumulative = quantityType.aggregationStyle == .cumulative
            let result = HealthRows()
            let sem = DispatchSemaphore(value: 0)
            let interval = DateComponents(day: 1)
            let anchor = Calendar.current.startOfDay(for: from)
            let query = HKStatisticsCollectionQuery(
                quantityType: quantityType,
                quantitySamplePredicate: predicate,
                options: cumulative ? .cumulativeSum : .discreteAverage,
                anchorDate: anchor,
                intervalComponents: interval
            )
            let unit = healthUnit(for: id, type: quantityType)
            query.initialResultsHandler = { _, collection, error in
                result.failure = error
                collection?.enumerateStatistics(from: from, to: to) { stats, _ in
                    let quantity = cumulative ? stats.sumQuantity() : stats.averageQuantity()
                    guard let quantity else { return }
                    result.rows.append([
                        "date": DateArg.format(stats.startDate),
                        "value": quantity.doubleValue(for: unit),
                    ])
                }
                sem.signal()
            }
            store.execute(query)
            guard sem.wait(timeout: .now() + 30) == .success else {
                throw DeviceError.message("Reading \(id) timed out.")
            }
            if let failure = result.failure { throw failure }
            return ["unit": unit.unitString, "bucket": "day", "points": result.rows]
        }

        let result = HealthRows()
        let sem = DispatchSemaphore(value: 0)
        let query = HKSampleQuery(
            sampleType: type,
            predicate: predicate,
            limit: 500,
            sortDescriptors: [NSSortDescriptor(key: HKSampleSortIdentifierStartDate, ascending: false)]
        ) { _, samples, error in
            result.failure = error
            for sample in samples ?? [] {
                var row: [String: Any] = [
                    "id": sample.uuid.uuidString,
                    "start": DateArg.format(sample.startDate),
                    "end": DateArg.format(sample.endDate),
                ]
                if let q = sample as? HKQuantitySample {
                    let unit = self.healthUnit(for: id, type: q.quantityType)
                    row["value"] = q.quantity.doubleValue(for: unit)
                    row["unit"] = unit.unitString
                } else if let c = sample as? HKCategorySample {
                    row["value"] = c.value
                }
                result.rows.append(row)
            }
            sem.signal()
        }
        store.execute(query)
        guard sem.wait(timeout: .now() + 30) == .success else {
            throw DeviceError.message("Reading \(id) timed out.")
        }
        if let failure = result.failure { throw failure }
        return ["samples": result.rows]
    }

    /// The unit each type is reported in. Wrong units are silent — a weight in
    /// pounds looks like a plausible weight — so the answer always names it.
    func healthUnit(for id: String, type: HKQuantityType) -> HKUnit {
        if let declared = Self.healthTypes.first(where: { $0.id == id })?.unit,
           declared != "—",
           type.is(compatibleWith: HKUnit(from: declared)) {
            return HKUnit(from: declared)
        }
        return type.is(compatibleWith: .count()) ? .count() : HKUnit(from: "")
    }

    func awaitHealthSave(_ sample: HKObject, store: HKHealthStore) throws {
        var failure: Error?
        let sem = DispatchSemaphore(value: 0)
        store.save(sample) { _, error in
            failure = error
            sem.signal()
        }
        guard sem.wait(timeout: .now() + 20) == .success else {
            throw DeviceError.message("Writing Health data timed out.")
        }
        if let failure { throw failure }
    }

    func deleteHealthSample(_ uuid: UUID, store: HKHealthStore) throws -> [String: Any] {
        var found: HKSample?
        var failure: Error?
        let sem = DispatchSemaphore(value: 0)
        let query = HKSampleQuery(
            sampleType: HKQuantityType(.bodyMass),
            predicate: HKQuery.predicateForObject(with: uuid),
            limit: 1,
            sortDescriptors: nil
        ) { _, samples, error in
            found = samples?.first
            failure = error
            sem.signal()
        }
        store.execute(query)
        _ = sem.wait(timeout: .now() + 20)
        if let failure { throw failure }
        guard let found else { return ["error": "No such sample, or it was not written by this app."] }
        var deleteFailure: Error?
        let sem2 = DispatchSemaphore(value: 0)
        store.delete(found) { _, error in
            deleteFailure = error
            sem2.signal()
        }
        _ = sem2.wait(timeout: .now() + 20)
        if let deleteFailure { throw deleteFailure }
        return ["ok": true, "deleted": uuid.uuidString]
    }

    /// What a HealthKit query's handler collects; the caller waits on a
    /// semaphore before reading it, so the two never touch it at once.
    final class HealthRows: @unchecked Sendable {
        var rows: [[String: Any]] = []
        var failure: Error?
    }
}
