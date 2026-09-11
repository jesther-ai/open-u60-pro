import SwiftUI
import os

private let logger = Logger(subsystem: "com.zte.companion", category: "SMS")

/// Hands a decoded JSON payload to a background task. Safe because the value is transferred
/// exactly once, is never mutated, and the sender drops it at the call site.
private struct UncheckedTransfer<T>: @unchecked Sendable {
    let value: T
}

@Observable
@MainActor
final class SMSViewModel {
    var conversations: [SMSConversation] = []
    var allMessages: [SMSMessage] = []
    var capacity: SMSCapacity = .empty
    var storageFilter: SMSStorageFilter = .all
    var isLoading = false
    var isSending = false
    var error: String?

    private let client: AgentClient
    private let authManager: AuthManager

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    // MARK: - Fetch

    func refresh() async {
        isLoading = true
        error = nil

        // Capacity is non-critical and independent, so let it fly alongside the list request.
        async let capacityResult = self.fetchCapacity()
        let messages = await fetchMessages()

        if let messages {
            allMessages = messages
            conversations = SMSParser.groupIntoConversations(messages)
        }

        if let cap = await capacityResult {
            capacity = cap
        }

        isLoading = false
    }

    private func fetchMessages() async -> [SMSMessage]? {
        do {
            let data = try await client.postJSON("/api/sms/list", body: [
                "page": 0,
                "data_per_page": 500,
                "mem_store": storageFilter.memStoreValue,
                "tags": 10,
                "order_by": "order by id desc"
            ])
            return await Self.parseMessagesOffMain(data)
        } catch {
            guard !error.isCancellation else { return nil }
            logger.error("fetchMessages: \(error.localizedDescription)")
            self.error = error.localizedDescription
            return nil
        }
    }

    /// A full page is 500 messages, each needing a UCS-2 decode and a date build. That is far
    /// too much work to do between two frames, so it runs off the main actor.
    private static func parseMessagesOffMain(_ data: [String: Any]) async -> [SMSMessage] {
        let payload = UncheckedTransfer(value: data)
        return await Task.detached(priority: .userInitiated) {
            SMSParser.parseMessages(payload.value)
        }.value
    }

    private func fetchCapacity() async -> SMSCapacity? {
        do {
            let data = try await client.getJSON("/api/sms/capacity")
            return SMSParser.parseCapacity(data)
        } catch {
            guard !error.isCancellation else { return nil }
            logger.warning("fetchCapacity: \(error.localizedDescription)")
            return nil
        }
    }

    // MARK: - Send

    func sendSMS(to number: String, message: String) async -> Bool {
        isSending = true
        error = nil

        let encodeType = SMSParser.getEncodeType(message)
        let body = SMSParser.encodeUCS2Hex(message)
        let smsTime = SMSParser.formatSMSTime()

        do {
            let _ = try await client.postJSON("/api/sms/send", body: [
                "number": number,
                "sms_time": smsTime,
                "message_body": body,
                "id": "-1",
                "encode_type": encodeType
            ])
            logger.info("SMS sent to \(number)")
            isSending = false
            await refresh()
            return true
        } catch {
            isSending = false
            guard !error.isCancellation else { return false }
            logger.error("sendSMS: \(error.localizedDescription)")
            self.error = "Failed to send: \(error.localizedDescription)"
            return false
        }
    }

    // MARK: - Delete

    func deleteMessages(ids: [Int]) async {
        let idStr = ids.map(String.init).joined(separator: ";") + ";"

        do {
            let _ = try await client.postJSON("/api/sms/delete", body: ["id": idStr])
            logger.info("Deleted SMS ids: \(idStr)")
            await refresh()
        } catch {
            guard !error.isCancellation else { return }
            logger.error("deleteMessages: \(error.localizedDescription)")
            self.error = "Delete failed: \(error.localizedDescription)"
        }
    }

    func deleteConversation(_ conversation: SMSConversation) async {
        let ids = conversation.messages.map(\.id)
        await deleteMessages(ids: ids)
    }

    // MARK: - Mark Read

    func markAsRead(ids: [Int]) async {
        guard !ids.isEmpty else { return }
        let idStr = ids.map(String.init).joined(separator: ";") + ";"

        do {
            let _ = try await client.postJSON("/api/sms/read", body: ["id": idStr, "tag": 0])
            // Update local state without full refresh
            let markedIds = Set(ids)
            for i in allMessages.indices where markedIds.contains(allMessages[i].id) {
                let msg = allMessages[i]
                allMessages[i] = SMSMessage(
                    id: msg.id, number: msg.number, content: msg.content,
                    date: msg.date, tag: .read, groupId: msg.groupId, memStore: msg.memStore
                )
            }
            conversations = SMSParser.groupIntoConversations(allMessages)
        } catch {
            guard !error.isCancellation else { return }
            logger.warning("markAsRead: \(error.localizedDescription)")
        }
    }
}
