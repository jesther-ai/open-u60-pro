import Foundation

@Observable
@MainActor
final class LANSpeedTestViewModel {
    var phase: String = "idle"
    var pingMs: Double?
    var downloadMbps: Double?
    var uploadMbps: Double?
    var liveSpeedMbps: Double = 0
    var progress: Double = 0
    var isRunning: Bool = false
    var error: String?

    private let client: AgentClient
    // @ObservationIgnored keeps this a plain stored property, so the nonisolated `deinit` can
    // cancel it. The @Observable macro would otherwise rewrite it into a main-actor accessor.
    @ObservationIgnored private var testTask: Task<Void, Never>?

    private let testSize = 20_000_000

    init(client: AgentClient) {
        self.client = client
    }

    deinit {
        testTask?.cancel()
    }

    func startTest() {
        guard !isRunning else { return }
        isRunning = true
        phase = "idle"
        pingMs = nil
        downloadMbps = nil
        uploadMbps = nil
        liveSpeedMbps = 0
        progress = 0
        error = nil

        let baseURL = client.baseURL
        let token = client.token
        let size = testSize
        let onProgress = downloadProgressSink()

        // Every transfer runs as `nonisolated static` work and the view model is only
        // re-acquired weakly in between, so nothing holds the model for the length of a
        // transfer. Popping the screen therefore deallocates it and `deinit` cancels this
        // task, while merely hiding the screen (a tab switch) leaves the run to finish.
        testTask = Task { [weak self] in
            do {
                try Task.checkCancellation()
                self?.beginPhase("ping")
                let ping = try await Self.measurePing(baseURL: baseURL, token: token)
                self?.finishPing(ping)

                try Task.checkCancellation()
                self?.beginPhase("download")
                let download = try await Self.measureDownload(
                    baseURL: baseURL,
                    token: token,
                    size: size,
                    onProgress: onProgress
                )
                self?.finishDownload(download)

                try Task.checkCancellation()
                self?.beginPhase("upload")
                let upload = try await Self.measureUpload(baseURL: baseURL, token: token, size: size)
                self?.finishUpload(upload)
            } catch {
                self?.finish(with: error)
            }
        }
    }

    func stopTest() {
        testTask?.cancel()
        testTask = nil
    }

    // MARK: - Phase transitions

    private func beginPhase(_ name: String) {
        phase = name
        liveSpeedMbps = 0
    }

    private func finishPing(_ ms: Double) {
        pingMs = ms
        progress = 0.2
    }

    private func finishDownload(_ mbps: Double) {
        downloadMbps = mbps
        progress = 0.6
    }

    private func finishUpload(_ mbps: Double) {
        uploadMbps = mbps
        progress = 1.0
        phase = "complete"
        liveSpeedMbps = 0
        isRunning = false
    }

    private func finish(with error: Error) {
        if error.isCancellation {
            phase = "cancelled"
        } else {
            self.error = error.localizedDescription
            phase = "error"
        }
        liveSpeedMbps = 0
        isRunning = false
    }

    /// Live-speed sink for the download phase. Captures the model weakly, so an in-flight
    /// transfer never keeps a dismissed screen alive.
    private func downloadProgressSink() -> @Sendable (Double, Double) -> Void {
        { [weak self] mbps, fraction in
            Task { @MainActor in
                guard let self else { return }
                self.liveSpeedMbps = mbps
                self.progress = 0.2 + fraction * 0.4
            }
        }
    }

    // MARK: - Ping

    private nonisolated static func measurePing(baseURL: String, token: String?) async throws -> Double {
        guard let url = URL(string: "\(baseURL)/api/lan/ping") else {
            throw URLError(.badURL)
        }

        var request = URLRequest(url: url)
        request.timeoutInterval = 5
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }

        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 5
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }

        var rtts: [Double] = []
        for _ in 0..<10 {
            try Task.checkCancellation()
            let start = CFAbsoluteTimeGetCurrent()
            let (_, _) = try await session.data(for: request)
            let rtt = (CFAbsoluteTimeGetCurrent() - start) * 1000
            rtts.append(rtt)
        }

        guard !rtts.isEmpty else { throw URLError(.cannotConnectToHost) }
        rtts.sort()
        return rtts[rtts.count / 2]
    }

    // MARK: - Download

    private nonisolated static func measureDownload(
        baseURL: String,
        token: String?,
        size: Int,
        onProgress: @escaping @Sendable (Double, Double) -> Void
    ) async throws -> Double {
        guard let url = URL(string: "\(baseURL)/api/lan/download?size=\(size)") else {
            throw URLError(.badURL)
        }

        var request = URLRequest(url: url)
        request.timeoutInterval = 120
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }

        let measurer = DownloadMeasurer(expectedSize: Int64(size), onProgress: onProgress)

        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 120
        config.timeoutIntervalForResource = 120
        let session = URLSession(configuration: config, delegate: measurer, delegateQueue: nil)
        defer { session.invalidateAndCancel() }

        let (_, _) = try await session.data(for: request, delegate: measurer)
        return measurer.finalMbps
    }

    // MARK: - Upload

    private nonisolated static func measureUpload(baseURL: String, token: String?, size: Int) async throws -> Double {
        guard let url = URL(string: "\(baseURL)/api/lan/upload") else {
            throw URLError(.badURL)
        }

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 120
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }

        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 120
        config.timeoutIntervalForResource = 120
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }

        let payload = Data(count: size)
        let start = CFAbsoluteTimeGetCurrent()
        let (data, _) = try await session.upload(for: request, from: payload)
        let clientElapsed = CFAbsoluteTimeGetCurrent() - start

        // Parse server-measured result (primary)
        if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let inner = json["data"] as? [String: Any],
           let serverMbps = inner["mbps"] as? Double {
            return serverMbps
        }

        // Fallback to client-side calculation
        return clientElapsed > 0 ? Double(size) * 8.0 / (clientElapsed * 1_000_000) : 0
    }
}

// MARK: - Download delegate

private final class DownloadMeasurer: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let startTime = CFAbsoluteTimeGetCurrent()
    private var totalReceived: Int64 = 0
    private var lastReport: CFAbsoluteTime = 0
    private let expectedSize: Int64
    private let onProgress: @Sendable (Double, Double) -> Void

    /// Progress is only forwarded this often; a fast LAN delivers chunks far quicker than the
    /// display can use them.
    private let reportInterval: CFAbsoluteTime = 0.1

    var finalMbps: Double {
        let elapsed = CFAbsoluteTimeGetCurrent() - startTime
        return elapsed > 0 ? Double(totalReceived) * 8.0 / (elapsed * 1_000_000) : 0
    }

    init(expectedSize: Int64, onProgress: @escaping @Sendable (Double, Double) -> Void) {
        self.expectedSize = expectedSize
        self.onProgress = onProgress
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        totalReceived += Int64(data.count)
        let now = CFAbsoluteTimeGetCurrent()
        let elapsed = now - startTime
        guard elapsed > 0.1, now - lastReport >= reportInterval else { return }
        lastReport = now
        let mbps = Double(totalReceived) * 8.0 / (elapsed * 1_000_000)
        let fraction = min(Double(totalReceived) / Double(expectedSize), 1.0)
        onProgress(mbps, fraction)
    }
}
