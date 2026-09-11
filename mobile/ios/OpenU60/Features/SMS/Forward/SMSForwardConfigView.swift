import SwiftUI

struct SMSForwardConfigView: View {
    @Bindable var viewModel: SMSForwardViewModel

    @State private var markRead = false
    @State private var deleteAfter = false

    var body: some View {
        List {
            if let msg = viewModel.message {
                Section {
                    Text(msg)
                        .font(.subheadline)
                        .foregroundStyle(viewModel.messageIsError ? .red : .green)
                        .textSelection(.enabled)
                }
            }

            Section("Settings") {
                Toggle("Enabled", isOn: Binding(
                    get: { viewModel.config.enabled },
                    set: { val in Task { await viewModel.toggleEnabled(val) } }
                ))

                Toggle("Mark Read After Forward", isOn: $markRead)

                Toggle("Delete After Forward", isOn: $deleteAfter)

                Button {
                    Task {
                        await viewModel.updateConfig(
                            enabled: viewModel.config.enabled,
                            pollIntervalSecs: viewModel.config.pollIntervalSecs,
                            markRead: markRead,
                            deleteAfter: deleteAfter
                        )
                    }
                } label: {
                    Text("Save Settings")
                        .frame(maxWidth: .infinity)
                }
            }

            if viewModel.config.rules.isEmpty && !viewModel.isLoading {
                Section {
                    ContentUnavailableView {
                        Label("No Rules", systemImage: "envelope.arrow.triangle.branch")
                    } description: {
                        Text("Add forwarding rules to automatically forward SMS messages to Telegram, Discord, webhooks, and more.")
                    }
                }
            }

            if !viewModel.config.rules.isEmpty {
                Section("Rules") {
                    ForEach(viewModel.config.rules) { rule in
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(rule.name)
                                    .font(.headline)
                                Text(filterSummary(rule.filter))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                Text(destinationSummary(rule.destination))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            .contentShape(Rectangle())
                            .onTapGesture {
                                viewModel.presentedSheet = .edit(rule)
                            }
                            .accessibilityElement(children: .combine)
                            .accessibilityAddTraits(.isButton)
                            .accessibilityHint("Edit rule")
                            Spacer()
                            Toggle(rule.name, isOn: Binding(
                                get: { rule.enabled },
                                set: { val in Task { await viewModel.toggleRule(id: rule.id, enabled: val) } }
                            ))
                            .labelsHidden()
                        }
                        .padding(.vertical, 2)
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) {
                                Task { await viewModel.deleteRule(id: rule.id) }
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                }
            }

            Section {
                Button {
                    viewModel.presentedSheet = .add
                } label: {
                    Label("New Rule", systemImage: "plus")
                }
            }

            Section {
                NavigationLink {
                    SMSForwardLogView(viewModel: viewModel)
                } label: {
                    Label("Forward Log", systemImage: "doc.text")
                }
            }

            if viewModel.lastForwardedId > 0 {
                Section {
                    HStack {
                        Text("Last Forwarded ID")
                            .foregroundStyle(.secondary)
                        Spacer()
                        Text("\(viewModel.lastForwardedId)")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .navigationTitle("SMS Forwarding")
        .refreshable {
            await viewModel.refresh()
            syncLocalState()
        }
        .overlay {
            if viewModel.isLoading {
                ProgressView()
                    .padding()
                    .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .task {
            await viewModel.refresh()
            syncLocalState()
        }
        .sheet(item: $viewModel.presentedSheet) { sheet in
            switch sheet {
            case .add:
                SMSForwardRuleFormView(viewModel: viewModel)
            case .edit(let rule):
                SMSForwardRuleFormView(viewModel: viewModel, editingRule: rule)
            }
        }
    }

    private func syncLocalState() {
        markRead = viewModel.config.markReadAfterForward
        deleteAfter = viewModel.config.deleteAfterForward
    }

    private func filterSummary(_ filter: SmsFilter) -> String {
        switch filter {
        case .all:
            return "All messages"
        case .sender(let patterns):
            return "Sender: \(patterns.joined(separator: ", "))"
        case .content(let keywords):
            return "Keywords: \(keywords.joined(separator: ", "))"
        case .senderAndContent(let patterns, let keywords):
            return "Sender: \(patterns.joined(separator: ", ")) + Keywords: \(keywords.joined(separator: ", "))"
        }
    }

    private func destinationSummary(_ dest: ForwardDestination) -> String {
        switch dest {
        case .telegram(_, let chatId, _):
            return "Telegram (chat: \(chatId))"
        case .webhook(let url, _, _):
            return "Webhook (\(url))"
        case .sms(let number):
            return "SMS (\(number))"
        case .ntfy(_, let topic, _):
            return "ntfy (\(topic))"
        case .discord:
            return "Discord"
        case .slack:
            return "Slack"
        }
    }
}
