import Foundation

/// What to tell the user about a core error, in their language. The core
/// never writes these sentences; it only says which kind of failure it was.
extension CoreError {
    var message: String {
        switch self {
        case .NoModel:
            return String(localized: "No model is selected. Add an endpoint in Settings and choose a model.")
        case .UnknownEndpoint:
            return String(localized: "This chat's endpoint no longer exists. Choose another model.")
        case .MissingKey(let name):
            return String(localized: "There is no API key for \(name). Add one in Settings.")
        case .Network(let detail):
            return String(localized: "Could not reach the model: \(detail)")
        case .Http(let status, let detail):
            return String(localized: "The endpoint answered \(String(status)): \(detail)")
        case .Unauthorized(let detail):
            return String(localized: "The endpoint rejected the API key: \(detail)")
        case .ContextTooLong:
            return String(localized: "This conversation is longer than the model accepts.")
        case .ContextNearlyFull(let used, let window, let canSummarize):
            return canSummarize
                ? String(localized: "This conversation is close to the model's limit (about \(String(used)) of \(String(window)) tokens). Summarize the earlier part to continue, or start a new chat.")
                : String(localized: "This conversation has reached the model's limit (about \(String(used)) of \(String(window)) tokens). Start a new chat to continue.")
        case .NoSuchTerminal:
            return String(localized: "That terminal is no longer open.")
        case .NothingToCompact:
            return String(localized: "There is nothing older than the last few messages to summarize yet.")
        case .Stalled(let seconds):
            return String(localized: "The model sent nothing for \(String(seconds)) seconds, so the turn was stopped.")
        case .EmptyResponse:
            return String(localized: "The model answered with nothing. Try again, or choose another model.")
        case .TooManyRounds(let rounds):
            return String(localized: "Stopped after \(String(rounds)) rounds of tool calls. Try splitting the task.")
        case .Loop(let tool):
            return String(localized: "The same \(tool) call kept giving the same result, so the turn was stopped.")
        case .Busy:
            return String(localized: "A reply is still running.")
        case .NothingToRetry:
            return String(localized: "There is no message to answer again.")
        case .NothingToResume:
            return String(localized: "The last reply finished; there is nothing to continue.")
        case .SessionNotFound:
            return String(localized: "This chat no longer exists.")
        case .Sandbox(let detail):
            return String(localized: "The Linux sandbox is not available: \(detail)")
        case .Storage(let detail):
            return String(localized: "Could not save: \(detail)")
        case .Protocol(let detail):
            return String(localized: "The endpoint sent something unexpected: \(detail)")
        case .Internal(let detail):
            return String(localized: "Something went wrong: \(detail)")
        }
    }
}
