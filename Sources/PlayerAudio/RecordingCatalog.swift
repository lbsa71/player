import Foundation
import CoreFoundation
import Darwin

/// Read-only, application-independent view of recording metadata. The default
/// scan never opens PCM files. `verifyAudio` additionally checks WAV structure,
/// without decoding or reading sample payloads. No work runs on an audio thread.
public enum RecordingCatalog {
    /// Analyzer can append substantial analysis to a recording sidecar. Parse one
    /// bounded sidecar at a time, then retain only a small metadata projection.
    public static let metadataLimit = 64 * 1024 * 1024
    public static let retainedMetadataLimit = 32 * 1024
    public static let boundaryToleranceSeconds = 2.0

    public static func scan(directory: URL, verifyAudio: Bool = false) throws -> [String: Any] {
        let root = directory.standardizedFileURL
        let rootInfo = try attributes(root)
        guard rootInfo.st_mode & S_IFMT == S_IFDIR else { throw CatalogError("Recording directory must be a real directory, not a symlink.") }
        let contents = try FileManager.default.contentsOfDirectory(at:root, includingPropertiesForKeys:nil)
        let names = Set(contents.map(\.lastPathComponent))
        let sidecars = names.filter { $0.hasSuffix(".wav.json") }.sorted()
        var entries: [[String:Any]] = []
        var issues: [[String:String]] = []
        for name in sidecars {
            let audioName = String(name.dropLast(5))
            var entry: [String:Any] = ["metadataFile":name,"file":audioName,"metadata":[:],"trackID":"", "audioPresent":false,"audioVerified":false,"status":"invalid"]
            autoreleasepool {
                do {
                    let data = try readMetadata(root.appendingPathComponent(name))
                    guard let metadata = try JSONSerialization.jsonObject(with:data) as? [String:Any] else { throw CatalogError("Metadata must be a JSON object.") }
                    let retained = compactMetadata(metadata)
                    entry["metadata"] = retained.metadata
                    entry["metadataOmittedKeys"] = retained.omitted
                    entry["trackID"] = metadata["trackID"] as? String ?? ""
                    guard let referenced = metadata["file"] as? String, referenced == audioName,
                          referenced != ".", referenced != "..", !referenced.contains("/"), !referenced.contains("\\") else {
                        throw CatalogError("Metadata file reference must match its WAV basename in this directory.")
                    }
                    let audio = root.appendingPathComponent(audioName)
                    let info = try attributes(audio)
                    guard info.st_mode & S_IFMT == S_IFREG else { throw CatalogError("WAV must be a regular file, not a symlink.") }
                    entry["audioPresent"] = true
                    entry["audioBytes"] = info.st_size
                    if let frames = finiteNumber(metadata["frames"]), frames >= 0,
                       Double(info.st_size) < frames * 8 {
                        throw CatalogError("WAV is smaller than the sample payload claimed by its metadata.")
                    }
                    if verifyAudio && !audioName.hasSuffix(".partial.wav") {
                        let facts = try inspectWAV(audio)
                        entry["frames"] = facts.frames
                        entry["sampleRate"] = facts.rate
                        entry["durationSeconds"] = facts.duration
                        guard let frames = finiteNumber(metadata["frames"]), frames == Double(facts.frames),
                              let rate = finiteNumber(metadata["sampleRate"]), rate == Double(facts.rate),
                              finiteNumber(metadata["channels"]) == 2 else {
                            throw CatalogError("WAV format or frame count disagrees with its metadata.")
                        }
                        entry["audioVerified"] = true
                    }
                    let assessment = classify(metadata, partial:audioName.hasSuffix(".partial.wav"))
                    entry["status"] = assessment.status
                    entry["reason"] = assessment.reason
                } catch {
                    entry["status"] = "invalid"; entry["reason"] = error.localizedDescription
                    issues.append(["file":name,"reason":error.localizedDescription])
                }
            }
            entries.append(entry)
        }
        // Files without sidecars are visible but never counted as complete.
        for name in names.filter({ $0.hasSuffix(".wav") && !names.contains($0 + ".json") }).sorted() {
            var entry: [String:Any] = ["metadataFile":"", "file":name,"metadata":[:],"trackID":"", "audioPresent":false,"audioVerified":false]
            do {
                let audio = root.appendingPathComponent(name)
                let info = try attributes(audio)
                guard info.st_mode & S_IFMT == S_IFREG else { throw CatalogError("WAV must be a regular file, not a symlink.") }
                entry["audioPresent"] = true; entry["audioBytes"] = info.st_size
                if verifyAudio && !name.hasSuffix(".partial.wav") {
                    let facts = try inspectWAV(audio)
                    entry["audioVerified"] = true; entry["frames"] = facts.frames
                    entry["sampleRate"] = facts.rate; entry["durationSeconds"] = facts.duration
                }
                entry["status"] = name.hasSuffix(".partial.wav") ? "incomplete" : "unverified"
                entry["reason"] = "No recording metadata is available."
            } catch {
                entry["status"] = "invalid"; entry["reason"] = error.localizedDescription
                issues.append(["file":name,"reason":error.localizedDescription])
            }
            entries.append(entry)
        }
        entries.sort { ($0["file"] as? String ?? "") < ($1["file"] as? String ?? "") }
        return ["schemaVersion":1,"completionPolicy":"observed-v1","boundaryToleranceSeconds":boundaryToleranceSeconds,
                "audioVerificationRequested":verifyAudio,"entries":entries,"issues":issues]
    }

    private static func classify(_ m: [String:Any], partial: Bool) -> (status:String,reason:String) {
        if partial || boolean(m["segmentFinalized"]) == false { return ("incomplete","The recording segment was not finalized.") }
        guard finiteNumber(m["recordingSchemaVersion"]) == 2,
              m["completionPolicy"] as? String == "observed-v1" else {
            return ("unverified","This metadata does not contain supported capture-coverage evidence.")
        }
        guard boolean(m["segmentFinalized"]) == true else { return ("incomplete","The recording segment was not finalized.") }
        guard let id = m["trackID"] as? String, !id.isEmpty,
              let expected = finiteNumber(m["expectedDurationSeconds"]), expected > 0,
              let start = finiteNumber(m["observedStartPositionSeconds"]), start >= 0,
              let end = finiteNumber(m["observedEndPositionSeconds"]), end >= start,
              let frames = finiteNumber(m["frames"]), frames > 0, frames.rounded() == frames,
              let rate = finiteNumber(m["sampleRate"]), rate > 0,
              let continuous = boolean(m["coverageContinuous"]) else {
            return ("unverified","Duration, identity, or coverage evidence is missing or invalid.")
        }
        let duration = frames / rate, tolerance = boundaryToleranceSeconds
        guard continuous, start <= tolerance, end >= expected-tolerance,
              duration >= expected-tolerance, duration <= expected+tolerance,
              start+duration >= expected-tolerance else {
            return ("incomplete","Capture does not cover the track continuously within the observed-boundary tolerance.")
        }
        return ("complete","Complete according to observed-v1 metadata (2-second boundary tolerance; not sample-accurate source verification).")
    }

    private static func finiteNumber(_ raw: Any?) -> Double? {
        guard let n = raw as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        return n.doubleValue.isFinite ? n.doubleValue : nil
    }
    private static func boolean(_ raw: Any?) -> Bool? {
        guard let n = raw as? NSNumber, CFGetTypeID(n) == CFBooleanGetTypeID() else { return nil }
        return n.boolValue
    }
    private static func compactMetadata(_ source: [String:Any]) -> (metadata:[String:Any], omitted:[String]) {
        let captureKeys = ["recordingSchemaVersion","completionPolicy","trackID","file","frames","sampleRate","channels",
                           "expectedDurationSeconds","observedStartPositionSeconds","observedEndPositionSeconds",
                           "coverageContinuous","segmentFinalized"]
        let ordered = captureKeys.filter { source[$0] != nil } + source.keys.filter { !captureKeys.contains($0) }.sorted()
        var retained: [String:Any] = [:], omitted: [String] = []
        var bytes = 2
        for key in ordered {
            // Analyzer output is a separate consumer's document, not capture evidence.
            guard key != "analyzer", let value = source[key] else { omitted.append(key); continue }
            guard let encoded = try? JSONSerialization.data(withJSONObject:[key:value]),
                  encoded.count + bytes <= retainedMetadataLimit else { omitted.append(key); continue }
            retained[key] = value
            bytes += encoded.count
        }
        return (retained,omitted.sorted())
    }
    private static func attributes(_ url: URL) throws -> stat {
        var result = stat()
        guard lstat(url.path,&result) == 0 else { throw CatalogError("File is unavailable (POSIX \(errno)).") }
        return result
    }
    private static func openRegular(_ url: URL) throws -> (FileHandle, UInt64) {
        let fd = Darwin.open(url.path,O_RDONLY | O_NOFOLLOW | O_NONBLOCK)
        guard fd >= 0 else { throw CatalogError("Cannot open regular file (POSIX \(errno)).") }
        var info = stat()
        guard fstat(fd,&info) == 0, info.st_mode & S_IFMT == S_IFREG, info.st_size >= 0 else {
            Darwin.close(fd); throw CatalogError("Only regular files are supported.")
        }
        return (FileHandle(fileDescriptor:fd,closeOnDealloc:true), UInt64(info.st_size))
    }
    private static func readMetadata(_ url: URL) throws -> Data {
        let (handle,size) = try openRegular(url); defer { try? handle.close() }
        guard size <= metadataLimit else { throw CatalogError("Metadata exceeds the 64 MiB limit.") }
        let data = try handle.read(upToCount:metadataLimit+1) ?? Data()
        guard data.count <= metadataLimit else { throw CatalogError("Metadata exceeds the 64 MiB limit.") }
        return data
    }
    private struct WAVFacts { let frames: UInt64; let rate: UInt32; var duration: Double { Double(frames)/Double(rate) } }
    private static func inspectWAV(_ url: URL) throws -> WAVFacts {
        let (handle,size) = try openRegular(url); defer { try? handle.close() }
        func read(_ position: UInt64,_ count: Int) throws -> Data {
            try handle.seek(toOffset:position)
            let result = try handle.read(upToCount:count) ?? Data()
            guard result.count == count else { throw CatalogError("Truncated WAV header.") }
            return result
        }
        func u16(_ d: Data,_ n: Int) -> UInt16 { UInt16(d[n]) | UInt16(d[n+1]) << 8 }
        func u32(_ d: Data,_ n: Int) -> UInt32 { UInt32(d[n]) | UInt32(d[n+1]) << 8 | UInt32(d[n+2]) << 16 | UInt32(d[n+3]) << 24 }
        let header = try read(0,12)
        guard header.prefix(4) == Data("RIFF".utf8), header.suffix(4) == Data("WAVE".utf8), UInt64(u32(header,4))+8 == size else {
            throw CatalogError("WAV RIFF header or file length is invalid.")
        }
        var position: UInt64 = 12, rate: UInt32?, frameCount: UInt64?, chunks = 0
        while position+8 <= size {
            chunks += 1
            guard chunks <= 1024 else { throw CatalogError("WAV contains too many chunks.") }
            let chunk = try read(position,8), length = UInt64(u32(chunk,4))
            let body = position+8, next = body+length+(length%2)
            guard next <= size else { throw CatalogError("Truncated WAV chunk.") }
            if chunk.prefix(4) == Data("fmt ".utf8) {
                guard rate == nil, length >= 16 else { throw CatalogError("Invalid WAV format chunk.") }
                let f = try read(body,16), r = u32(f,4)
                guard u16(f,0) == 3, u16(f,2) == 2, r > 0, UInt64(u32(f,8)) == UInt64(r)*8,
                      u16(f,12) == 8, u16(f,14) == 32 else { throw CatalogError("WAV must be stereo IEEE Float32 PCM.") }
                rate = r
            } else if chunk.prefix(4) == Data("data".utf8) {
                guard frameCount == nil, length % 8 == 0 else { throw CatalogError("Invalid WAV data chunk.") }
                frameCount = length/8
            }
            position = next
        }
        guard position == size, let rate, let frames = frameCount else { throw CatalogError("WAV format or data chunk is missing.") }
        return WAVFacts(frames:frames,rate:rate)
    }
    private struct CatalogError: LocalizedError {
        let message: String
        init(_ message:String) { self.message = message }
        var errorDescription: String? { message }
    }
}
