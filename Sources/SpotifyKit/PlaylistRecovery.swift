import Foundation

/// A playlist recovery policy with no networking, catalog access, or audio work.
/// The caller starts playback at the playlist beginning, then starts this policy
/// once the command succeeds. Feed only fresh, serialized playback observations.
/// Execute `.skip` or `.restart` once; success does not clear the pending command.
/// Only observed playback confirms it. Cancel on transport failure or user action.
public struct PlaylistRecovery: Sendable {
    public struct Observation: Sendable {
        public let contextURI: String?
        public let deviceID: String?
        public let trackID: String?
        public let positionSeconds: Double?
        public let isPlaying: Bool
        public let shuffle: Bool?
        public let repeatMode: String?
        public let stale: Bool

        public init(contextURI: String?, deviceID: String?, trackID: String?, positionSeconds: Double?,
                    isPlaying: Bool, shuffle: Bool?, repeatMode: String?, stale: Bool = false) {
            self.contextURI = contextURI; self.deviceID = deviceID; self.trackID = trackID
            self.positionSeconds = positionSeconds; self.isPlaying = isPlaying
            self.shuffle = shuffle; self.repeatMode = repeatMode; self.stale = stale
        }

        public init(playback: SpotifyPlayback?, stale: Bool = false) {
            self.init(contextURI:playback?.context?.uri, deviceID:playback?.device?.id,
                      trackID:playback?.currently_playing_type == "track" ? playback?.item?.id : nil,
                      positionSeconds:playback?.progress_ms.map { Double($0)/1000 },
                      isPlaying:playback?.is_playing ?? false, shuffle:playback?.shuffle_state,
                      repeatMode:playback?.repeat_state, stale:stale)
        }
    }

    public enum Action: Equatable, Sendable {
        /// Capture is allowed for this observation. Repeated capture actions are expected.
        case capture
        /// Suppress capture and issue a single next-track command.
        case skip
        /// Suppress capture and issue a single seek-to-zero command.
        case restart
        /// Suppress capture while waiting for observed confirmation.
        case wait
        /// Recovery is inactive. The caller decides whether playback itself should pause.
        case stop(String)
    }

    private enum Command: Sendable { case skip, restart }
    private struct Pending: Sendable {
        let command: Command
        let trackID: String
        let issuedAt: TimeInterval
        var greatestPosition: Double
    }
    private enum Phase: Sendable {
        case inactive(String), ready, capturing(trackID:String, position:Double), pending(Pending)
    }
    public static let startToleranceSeconds = 2.0
    public static let commandTimeoutSeconds = 10.0
    private var phase: Phase = .inactive("Playlist recovery has not started.")
    private var playlistURI = ""
    private var deviceID = ""
    private var lastTime: TimeInterval = 0

    public init() {}

    public var isActive: Bool {
        if case .inactive = phase { return false }
        return true
    }
    public var isCapturing: Bool {
        if case .capturing = phase { return true }
        return false
    }

    /// `now` must come from one monotonic or reference-time clock throughout a run.
    public mutating func start(playlistURI: String, deviceID: String, now: TimeInterval) {
        self.playlistURI = playlistURI; self.deviceID = deviceID; lastTime = now
        phase = (!playlistURI.isEmpty && !deviceID.isEmpty && now.isFinite)
            ? .ready : .inactive("Playlist, device, and clock are required for recovery.")
    }

    @discardableResult
    public mutating func cancel(reason: String) -> Action {
        phase = .inactive(reason)
        return .stop(reason)
    }

    public mutating func observe(_ observation: Observation, completedTrackIDs: Set<String>, now: TimeInterval) -> Action {
        if case .inactive(let reason) = phase { return .stop(reason) }
        guard now.isFinite, now >= lastTime else { return cancel(reason:"The playback observation clock is invalid.") }
        lastTime = now
        guard !observation.stale else { return cancel(reason:"Playback metadata is stale; recovery stopped.") }
        guard let context = observation.contextURI, let device = observation.deviceID,
              let shuffle = observation.shuffle, let repeatMode = observation.repeatMode else {
            return cancel(reason:"Playback context, device, or mode metadata is unavailable.")
        }
        guard context == playlistURI else { return cancel(reason:"The active playlist changed.") }
        guard device == deviceID else { return cancel(reason:"The Spotify playback device changed.") }
        guard !shuffle, repeatMode == "off" else { return cancel(reason:"Shuffle or repeat changed; recovery stopped.") }
        guard observation.isPlaying else { return cancel(reason:"Playback paused or ended.") }
        guard let track = observation.trackID, !track.isEmpty,
              let position = observation.positionSeconds, position.isFinite, position >= 0 else {
            return cancel(reason:"Track identity or playback position is unavailable.")
        }

        switch phase {
        case .inactive(let reason): return .stop(reason)
        case .ready:
            return beginVisit(track:track, position:position, completed:completedTrackIDs, now:now)
        case .capturing(let previousTrack, let previousPosition):
            // A catalog refresh can complete an earlier duplicate of the current
            // track. Once capture starts, finish this visit instead of skipping it.
            if track == previousTrack && previousPosition-position <= Self.startToleranceSeconds {
                phase = .capturing(trackID:track, position:position)
                return .capture
            }
            return beginVisit(track:track, position:position, completed:completedTrackIDs, now:now)
        case .pending(var pending):
            guard now-pending.issuedAt < Self.commandTimeoutSeconds else {
                return cancel(reason:"Spotify did not confirm the recovery command within 10 seconds.")
            }
            switch pending.command {
            case .skip:
                let reset = position <= Self.startToleranceSeconds && pending.greatestPosition-position > Self.startToleranceSeconds
                if track != pending.trackID || reset {
                    // A new pending skip starts with the new visit's position.
                    // Repeated low-position samples cannot confirm it again.
                    return beginVisit(track:track, position:position, completed:completedTrackIDs, now:now)
                }
            case .restart:
                guard track == pending.trackID else { return cancel(reason:"Track changed before its restart was confirmed.") }
                if position <= Self.startToleranceSeconds && pending.greatestPosition-position >= 1 {
                    return beginVisit(track:track, position:position, completed:completedTrackIDs, now:now)
                }
            }
            pending.greatestPosition = max(pending.greatestPosition, position)
            phase = .pending(pending)
            return .wait
        }
    }

    private mutating func beginVisit(track: String, position: Double, completed: Set<String>, now: TimeInterval) -> Action {
        if completed.contains(track) {
            phase = .pending(Pending(command:.skip, trackID:track, issuedAt:now, greatestPosition:position))
            return .skip
        }
        if position > Self.startToleranceSeconds {
            phase = .pending(Pending(command:.restart, trackID:track, issuedAt:now, greatestPosition:position))
            return .restart
        }
        phase = .capturing(trackID:track, position:position)
        return .capture
    }
}
