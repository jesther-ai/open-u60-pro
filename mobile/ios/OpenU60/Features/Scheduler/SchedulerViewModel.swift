import SwiftUI

@Observable
@MainActor
final class SchedulerViewModel {
    var jobs: [SchedulerJob] = []
    var isLoading = false
    var message: String?
    var messageIsError = false
    var presentedSheet: Sheet?

    enum Sheet: Identifiable {
        case add
        case edit(SchedulerJob)
        var id: String {
            switch self {
            case .add: "add"
            case .edit(let job): "edit-\(job.id)"
            }
        }
    }

    private let client: AgentClient
    private let authManager: AuthManager

    /// Wire format for schedule times. The agent compares the stored string byte-for-byte against
    /// a Rust-formatted `"{:02}:{:02}"`, so the formatter must not emit the device's native digits.
    private static let wireTimeFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "HH:mm"
        return formatter
    }()

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func refresh() async {
        isLoading = true
        message = nil
        do {
            let jobsArray = try await client.getJSONArray("/api/scheduler/jobs")
            jobs = jobsArray.compactMap { SchedulerJob.parse($0) }
        } catch {
            if !error.isCancellation {
                showMessage("Failed to load: \(error.localizedDescription)", isError: true)
            }
        }
        isLoading = false
    }

    func createJob(name: String, template: ActionTemplate, scheduleType: String,
                   actionTime: Date, days: Set<Int>, onceDate: Date,
                   restoreEnabled: Bool, restoreTime: Date) async {
        isLoading = true
        var body: [String: Any] = ["name": name]

        var action: [String: Any] = ["method": template.method, "path": template.path]
        if let actionBody = template.actionBody {
            action["body"] = actionBody
        }
        body["action"] = action

        if scheduleType == "once" {
            body["schedule"] = ["type": "once", "at": Int(onceDate.timeIntervalSince1970)]
        } else {
            body["schedule"] = [
                "type": "recurring",
                "time": Self.wireTimeFormatter.string(from: actionTime),
                "days": Array(days).sorted()
            ] as [String: Any]
        }

        if restoreEnabled && template.supportsRestore {
            var restore: [String: Any] = ["time": Self.wireTimeFormatter.string(from: restoreTime)]
            if let restoreBody = template.restoreBody {
                restore["body"] = restoreBody
            }
            body["restore"] = restore
        }

        do {
            let _ = try await client.postJSON("/api/scheduler/jobs", body: body)
            showMessage("Automation created", isError: false)
            await refresh()
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func updateJob(id: Int, enabled: Bool, name: String, template: ActionTemplate,
                   scheduleType: String, actionTime: Date, days: Set<Int>, onceDate: Date,
                   restoreEnabled: Bool, restoreTime: Date) async {
        isLoading = true
        var body: [String: Any] = ["id": id, "name": name, "enabled": enabled]

        var action: [String: Any] = ["method": template.method, "path": template.path]
        if let actionBody = template.actionBody {
            action["body"] = actionBody
        }
        body["action"] = action

        if scheduleType == "once" {
            body["schedule"] = ["type": "once", "at": Int(onceDate.timeIntervalSince1970)]
        } else {
            body["schedule"] = [
                "type": "recurring",
                "time": Self.wireTimeFormatter.string(from: actionTime),
                "days": Array(days).sorted()
            ] as [String: Any]
        }

        if restoreEnabled && template.supportsRestore {
            var restore: [String: Any] = ["time": Self.wireTimeFormatter.string(from: restoreTime)]
            if let restoreBody = template.restoreBody {
                restore["body"] = restoreBody
            }
            body["restore"] = restore
        }

        do {
            let _ = try await client.putJSON("/api/scheduler/jobs", body: body)
            showMessage("Automation updated", isError: false)
            await refresh()
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func deleteJob(id: Int) async {
        do {
            let _ = try await client.deleteJSON("/api/scheduler/jobs", body: ["id": id])
            jobs.removeAll { $0.id == id }
            showMessage("Automation deleted", isError: false)
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
    }

    func toggleJob(id: Int, enabled: Bool) async {
        do {
            let _ = try await client.putJSON("/api/scheduler/jobs/toggle", body: ["id": id, "enabled": enabled])
            if let idx = jobs.firstIndex(where: { $0.id == id }) {
                jobs[idx].enabled = enabled
            }
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
    }

    private func showMessage(_ text: String, isError: Bool) {
        message = text
        messageIsError = isError
    }
}
