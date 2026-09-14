import SwiftUI

struct SMSForwardRuleFormView: View {
    @Bindable var viewModel: SMSForwardViewModel
    let editingRule: ForwardRule?
    @Environment(\.dismiss) private var dismiss

    @State private var draft: RuleDraft

    init(viewModel: SMSForwardViewModel, editingRule: ForwardRule? = nil) {
        self.viewModel = viewModel
        self.editingRule = editingRule
        _draft = State(initialValue: RuleDraft(rule: editingRule))
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Name") {
                    TextField("Rule name", text: $draft.name)
                }

                Section("Filter") {
                    Picker("Type", selection: $draft.filterType) {
                        Text("All Messages").tag("all")
                        Text("By Sender").tag("sender")
                        Text("By Content").tag("content")
                        Text("Sender + Content").tag("sender_and_content")
                    }

                    if draft.filterType == "sender" || draft.filterType == "sender_and_content" {
                        TextField("Sender patterns (comma-separated)", text: $draft.senderPatterns)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }

                    if draft.filterType == "content" || draft.filterType == "sender_and_content" {
                        TextField("Keywords (comma-separated)", text: $draft.contentKeywords)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                }

                Section("Destination") {
                    Picker("Type", selection: $draft.destType) {
                        Text("Telegram").tag("telegram")
                        Text("Webhook").tag("webhook")
                        Text("SMS").tag("sms")
                        Text("ntfy").tag("ntfy")
                        Text("Discord").tag("discord")
                        Text("Slack").tag("slack")
                    }

                    switch draft.destType {
                    case "telegram":
                        TextField("Bot Token", text: $draft.botToken)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        TextField("Chat ID", text: $draft.chatId)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        Toggle("Silent", isOn: $draft.silent)
                    case "webhook":
                        TextField("URL", text: $draft.webhookUrl)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .keyboardType(.URL)
                        Picker("Method", selection: $draft.webhookMethod) {
                            Text("POST").tag("POST")
                            Text("PUT").tag("PUT")
                        }
                        TextField("Headers (name: value, one per line)", text: $draft.webhookHeaders, axis: .vertical)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .lineLimit(3...6)
                    case "sms":
                        TextField("Forward to number", text: $draft.forwardNumber)
                            .keyboardType(.phonePad)
                    case "ntfy":
                        TextField("Server URL", text: $draft.ntfyUrl)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .keyboardType(.URL)
                        TextField("Topic", text: $draft.ntfyTopic)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        TextField("Token (optional)", text: $draft.ntfyToken)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    case "discord":
                        TextField("Webhook URL", text: $draft.discordUrl)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .keyboardType(.URL)
                    case "slack":
                        TextField("Webhook URL", text: $draft.slackUrl)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .keyboardType(.URL)
                    default:
                        EmptyView()
                    }
                }

                Section {
                    Button {
                        Task { await viewModel.testDestination(draft.destination) }
                    } label: {
                        Label("Test Destination", systemImage: "paperplane")
                    }
                    .disabled(!draft.isDestinationValid)
                }

                Section {
                    Button {
                        Task {
                            await save()
                            dismiss()
                        }
                    } label: {
                        Text("Save")
                            .frame(maxWidth: .infinity)
                    }
                    .disabled(draft.name.isEmpty || !draft.isDestinationValid)
                }
            }
            .navigationTitle(editingRule != nil ? "Edit Rule" : "New Rule")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }

    // MARK: - Actions

    private func save() async {
        if let rule = editingRule {
            await viewModel.updateRule(id: rule.id, name: draft.name, enabled: rule.enabled,
                                       filter: draft.filter, destination: draft.destination)
        } else {
            await viewModel.createRule(name: draft.name, filter: draft.filter, destination: draft.destination)
        }
    }
}

// MARK: - Draft

extension SMSForwardRuleFormView {

    /// Every editable field of the form in one value, so the view needs a single `@State` and
    /// a single initializer instead of seventeen of each.
    struct RuleDraft {
        var name = ""

        var filterType = "all"
        var senderPatterns = ""
        var contentKeywords = ""

        var destType = "telegram"

        // Telegram
        var botToken = ""
        var chatId = ""
        var silent = false

        // Webhook
        var webhookUrl = ""
        var webhookMethod = "POST"
        var webhookHeaders = ""

        // SMS
        var forwardNumber = ""

        // ntfy
        var ntfyUrl = "https://ntfy.sh"
        var ntfyTopic = ""
        var ntfyToken = ""

        // Discord
        var discordUrl = ""

        // Slack
        var slackUrl = ""

        /// Fields start at their blank-form defaults, so an edited rule only overwrites the
        /// ones it actually carries and switching destination in the picker still finds the
        /// same defaults the "New Rule" form shows.
        init(rule: ForwardRule?) {
            guard let rule else { return }
            name = rule.name

            switch rule.filter {
            case .all:
                filterType = "all"
            case .sender(let patterns):
                filterType = "sender"
                senderPatterns = patterns.joined(separator: ", ")
            case .content(let keywords):
                filterType = "content"
                contentKeywords = keywords.joined(separator: ", ")
            case .senderAndContent(let patterns, let keywords):
                filterType = "sender_and_content"
                senderPatterns = patterns.joined(separator: ", ")
                contentKeywords = keywords.joined(separator: ", ")
            }

            switch rule.destination {
            case .telegram(let token, let chat, let isSilent):
                destType = "telegram"
                botToken = token
                chatId = chat
                silent = isSilent
            case .webhook(let url, let method, let headers):
                destType = "webhook"
                webhookUrl = url
                webhookMethod = method
                webhookHeaders = headers.map { "\($0.0): \($0.1)" }.joined(separator: "\n")
            case .sms(let number):
                destType = "sms"
                forwardNumber = number
            case .ntfy(let url, let topic, let token):
                destType = "ntfy"
                ntfyUrl = url
                ntfyTopic = topic
                ntfyToken = token ?? ""
            case .discord(let url):
                destType = "discord"
                discordUrl = url
            case .slack(let url):
                destType = "slack"
                slackUrl = url
            }
        }

        var isDestinationValid: Bool {
            switch destType {
            case "telegram":
                return !botToken.isEmpty && !chatId.isEmpty
            case "webhook":
                return !webhookUrl.isEmpty
            case "sms":
                return !forwardNumber.isEmpty
            case "ntfy":
                return !ntfyUrl.isEmpty && !ntfyTopic.isEmpty
            case "discord":
                return !discordUrl.isEmpty
            case "slack":
                return !slackUrl.isEmpty
            default:
                return false
            }
        }

        var filter: SmsFilter {
            switch filterType {
            case "sender":
                return .sender(patterns: Self.splitCSV(senderPatterns))
            case "content":
                return .content(keywords: Self.splitCSV(contentKeywords))
            case "sender_and_content":
                return .senderAndContent(patterns: Self.splitCSV(senderPatterns),
                                         keywords: Self.splitCSV(contentKeywords))
            default:
                return .all
            }
        }

        var destination: ForwardDestination {
            switch destType {
            case "telegram":
                return .telegram(botToken: botToken.trimmingCharacters(in: .whitespaces),
                                 chatId: chatId.trimmingCharacters(in: .whitespaces),
                                 silent: silent)
            case "webhook":
                return .webhook(url: webhookUrl.trimmingCharacters(in: .whitespaces),
                                method: webhookMethod,
                                headers: Self.parseHeaders(webhookHeaders))
            case "sms":
                return .sms(forwardNumber: forwardNumber.trimmingCharacters(in: .whitespaces))
            case "ntfy":
                let token = ntfyToken.trimmingCharacters(in: .whitespaces)
                return .ntfy(url: ntfyUrl.trimmingCharacters(in: .whitespaces),
                             topic: ntfyTopic.trimmingCharacters(in: .whitespaces),
                             token: token.isEmpty ? nil : token)
            case "discord":
                return .discord(webhookUrl: discordUrl.trimmingCharacters(in: .whitespaces))
            case "slack":
                return .slack(webhookUrl: slackUrl.trimmingCharacters(in: .whitespaces))
            default:
                return .telegram(botToken: "", chatId: "", silent: false)
            }
        }

        private static func splitCSV(_ text: String) -> [String] {
            text.split(separator: ",")
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }
        }

        private static func parseHeaders(_ text: String) -> [(String, String)] {
            text.split(separator: "\n").compactMap { line in
                let parts = line.split(separator: ":", maxSplits: 1)
                guard parts.count == 2 else { return nil }
                return (parts[0].trimmingCharacters(in: .whitespaces),
                        parts[1].trimmingCharacters(in: .whitespaces))
            }
        }
    }
}
