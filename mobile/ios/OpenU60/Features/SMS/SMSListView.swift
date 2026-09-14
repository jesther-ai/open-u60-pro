import SwiftUI

struct SMSListView: View {
    var viewModel: SMSViewModel
    let client: AgentClient
    let authManager: AuthManager
    @State private var showCompose = false
    @State private var showCall = false
    @State private var searchText = ""

    /// Reference point every row's relative timestamp is measured from.
    @State private var now = Date()

    /// One coarse ticker for the whole list instead of a per-row `Text(_, style: .relative)`
    /// timer — minute-granularity labels don't need a faster cadence. Held in `@State` so it
    /// survives body re-evaluation; `.common` mode keeps it running while scrolling.
    @State private var ticker = Timer.publish(every: 30, on: .main, in: .common).autoconnect()

    private var filteredConversations: [SMSConversation] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return viewModel.conversations }
        return viewModel.conversations.filter { conversation in
            conversation.number.localizedCaseInsensitiveContains(query)
                || conversation.messages.contains { $0.content.localizedCaseInsensitiveContains(query) }
        }
    }

    var body: some View {
        NavigationStack {
            List {
                Picker("Storage", selection: Binding(
                    get: { viewModel.storageFilter },
                    set: { newValue in
                        viewModel.storageFilter = newValue
                        Task { await viewModel.refresh() }
                    }
                )) {
                    ForEach(SMSStorageFilter.allCases, id: \.self) { filter in
                        Text(filter.rawValue).tag(filter)
                    }
                }
                .pickerStyle(.segmented)
                .listRowInsets(EdgeInsets(top: 8, leading: 16, bottom: 8, trailing: 16))

                if let error = viewModel.error {
                    Section {
                        Text(error)
                            .foregroundStyle(.red)
                            .font(.caption)
                    }
                }

                if viewModel.capacity.nvTotal > 0 {
                    Section {
                        HStack {
                            Text("Storage")
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text("\(viewModel.capacity.nvUsed)/\(viewModel.capacity.nvTotal)")
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Section("\(filteredConversations.count) Conversations") {
                    ForEach(filteredConversations) { conversation in
                        NavigationLink(value: conversation.id) {
                            SMSConversationRow(conversation: conversation, now: now)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                            Button(role: .destructive) {
                                Task { await viewModel.deleteConversation(conversation) }
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                }
            }
            .navigationTitle("Messages")
            .navigationDestination(for: String.self) { conversationId in
                if let conversation = viewModel.conversations.first(where: { $0.id == conversationId }) {
                    SMSConversationView(viewModel: viewModel, conversation: conversation)
                }
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    NavigationLink {
                        SMSForwardConfigView(viewModel: SMSForwardViewModel(client: client, authManager: authManager))
                    } label: {
                        Image(systemName: "gearshape")
                    }
                    .accessibilityLabel("SMS Forwarding")
                }
                ToolbarItem(placement: .primaryAction) {
                    HStack(spacing: 24) {
                        Button {
                            showCall = true
                        } label: {
                            Image(systemName: "phone")
                        }
                        .accessibilityLabel("Call")
                        Button {
                            showCompose = true
                        } label: {
                            Image(systemName: "square.and.pencil")
                        }
                        .accessibilityLabel("New Message")
                    }
                }
            }
            .searchable(text: $searchText, prompt: "Search messages")
            .refreshable { await viewModel.refresh() }
            .overlay {
                if viewModel.isLoading && viewModel.conversations.isEmpty {
                    ProgressView()
                } else if !viewModel.isLoading && viewModel.conversations.isEmpty && viewModel.error == nil {
                    ContentUnavailableView(
                        "No Messages",
                        systemImage: "message",
                        description: Text("SMS messages will appear here")
                    )
                } else if filteredConversations.isEmpty && !searchText.isEmpty {
                    ContentUnavailableView.search(text: searchText)
                }
            }
            .sheet(isPresented: $showCompose) {
                SMSComposeView(viewModel: viewModel)
            }
            .fullScreenCover(isPresented: $showCall) {
                CallView(viewModel: CallViewModel(client: client, authManager: authManager))
            }
            .onReceive(ticker) { now = $0 }
            .task { await viewModel.refresh() }
        }
    }
}

// MARK: - Conversation Row

private struct SMSConversationRow: View {
    let conversation: SMSConversation
    /// Bumped by the list's ticker, so the timestamp keeps up without a timer of its own.
    let now: Date

    private static let relativeFormatter: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter
    }()

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(conversation.number)
                        .font(.body.weight(conversation.unreadCount > 0 ? .bold : .medium))
                        .lineLimit(1)
                    Spacer()
                    // Formatted against `now` rather than `style: .relative`, which would install
                    // a per-second refresh timer on every visible row.
                    Text(Self.relativeFormatter.localizedString(for: conversation.latestTime, relativeTo: now))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                HStack {
                    Text(conversation.latestMessage)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                    Spacer()
                    if conversation.unreadCount > 0 {
                        Text("\(conversation.unreadCount)")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(.blue, in: Capsule())
                            .accessibilityLabel("\(conversation.unreadCount) unread")
                    }
                }
            }
        }
        .padding(.vertical, 2)
    }
}
