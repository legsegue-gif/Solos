//  Contacts, looked up by name.

import Foundation
import Contacts

extension DeviceTools {
    func contacts(_ input: [String: Any]) throws -> [String: Any] {
        let store = CNContactStore()
        try requestContactsAccess(store)
        guard let query = input["query"] as? String, !query.isEmpty else {
            return ["error": "`query` is required."]
        }
        let limit = input["limit"] as? Int ?? 10
        let keys: [CNKeyDescriptor] = [
            CNContactFormatter.descriptorForRequiredKeys(for: .fullName),
            CNContactPhoneNumbersKey as CNKeyDescriptor,
            CNContactEmailAddressesKey as CNKeyDescriptor,
        ]
        let found = try store.unifiedContacts(
            matching: CNContact.predicateForContacts(matchingName: query), keysToFetch: keys)
        let items = found.prefix(limit).map { c -> [String: Any] in
            [
                "name": CNContactFormatter.string(from: c, style: .fullName) ?? "",
                "phones": c.phoneNumbers.map { $0.value.stringValue },
                "emails": c.emailAddresses.map { $0.value as String },
            ]
        }
        return ["items": Array(items), "total": found.count]
    }

    func requestContactsAccess(_ store: CNContactStore) throws {
        switch CNContactStore.authorizationStatus(for: .contacts) {
        case .authorized: return
        case .denied, .restricted: throw DeviceError.message("Access to contacts is denied; it can be allowed in Settings.")
        default: break
        }
        let semaphore = DispatchSemaphore(value: 0)
        var granted = false
        store.requestAccess(for: .contacts) { ok, _ in
            granted = ok
            semaphore.signal()
        }
        _ = semaphore.wait(timeout: .now() + Self.personDeadline)
        if !granted { throw DeviceError.message("The user did not allow access to contacts.") }
    }
}
