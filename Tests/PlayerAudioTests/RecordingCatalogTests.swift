import XCTest
@testable import PlayerAudio

final class RecordingCatalogTests: XCTestCase {
    private func folder() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at:root,withIntermediateDirectories:true)
        addTeardownBlock { try FileManager.default.removeItem(at:root) }
        return root
    }
    private func metadata(_ name: String, frames: Int = 80000) -> [String:Any] {
        ["recordingSchemaVersion":2,"completionPolicy":"observed-v1","trackID":"spotify-track-123",
         "file":name,"frames":frames,"sampleRate":8000,"channels":2,"expectedDurationSeconds":10.0,
         "observedStartPositionSeconds":0.0,"observedEndPositionSeconds":10.0,
         "coverageContinuous":true,"segmentFinalized":true,"album":"Album","customFutureField":["hello":"world"]]
    }
    private func write(_ root:URL,_ name:String, metadata m:[String:Any]?, frames:UInt64 = 80000) throws {
        var data = FloatWAVFile.header(rate:8000,frames:frames)
        data.append(Data(count:Int(frames)*8))
        try data.write(to:root.appendingPathComponent(name))
        if let m { try writeMetadata(root,name,m) }
    }
    private func writeMetadata(_ root:URL,_ name:String,_ m:[String:Any]) throws {
        try JSONSerialization.data(withJSONObject:m).write(to:root.appendingPathComponent(name+".json"))
    }
    private func entries(_ root:URL,verify:Bool = false) throws -> [[String:Any]] {
        try XCTUnwrap(RecordingCatalog.scan(directory:root,verifyAudio:verify)["entries"] as? [[String:Any]])
    }
    func testMetadataFirstListingPreservesRawFieldsAndDoesNotReadWAVHeaders() throws {
        let root = try folder(), name = "Album - Artist - Track.wav"
        let m = metadata(name)
        try Data(count:640058).write(to:root.appendingPathComponent(name)) // deliberately not a WAV
        try writeMetadata(root,name,m)
        let entry = try XCTUnwrap(entries(root).first)
        XCTAssertEqual(entry["status"] as? String,"complete")
        XCTAssertEqual(entry["audioVerified"] as? Bool,false)
        XCTAssertEqual(entry["metadataFile"] as? String,name+".json")
        XCTAssertEqual(entry["trackID"] as? String,"spotify-track-123")
        let raw = try XCTUnwrap(entry["metadata"] as? [String:Any])
        XCTAssertEqual((raw["customFutureField"] as? [String:String])?["hello"],"world")
        XCTAssertEqual(try entries(root,verify:true).first?["status"] as? String,"invalid")
    }
    func testVerifiedCompleteCaptureMatchesMetadataAndHeader() throws {
        let root = try folder(), name = "Full.wav"
        try write(root,name,metadata:metadata(name))
        let entry = try XCTUnwrap(entries(root,verify:true).first)
        XCTAssertEqual(entry["status"] as? String,"complete")
        XCTAssertEqual(entry["audioVerified"] as? Bool,true)
        XCTAssertEqual(entry["frames"] as? UInt64,80000)
        XCTAssertEqual(entry["durationSeconds"] as? Double,10)
    }
    func testLegacyAndUnknownSchemaNeverCountAsComplete() throws {
        let root = try folder()
        var legacy = metadata("Legacy.wav"); legacy.removeValue(forKey:"recordingSchemaVersion")
        var future = metadata("Future.wav"); future["recordingSchemaVersion"] = 7
        try write(root,"Legacy.wav",metadata:legacy)
        try write(root,"Future.wav",metadata:future)
        XCTAssertEqual(try entries(root,verify:true).compactMap { $0["status"] as? String },["unverified","unverified"])
    }
    func testMissingAndMalformedEvidenceNeverCountAsComplete() throws {
        let root = try folder()
        var missing = metadata("MissingEvidence.wav"); missing.removeValue(forKey:"observedEndPositionSeconds")
        var malformed = metadata("MalformedEvidence.wav"); malformed["coverageContinuous"] = 1
        try write(root,"MissingEvidence.wav",metadata:missing)
        try write(root,"MalformedEvidence.wav",metadata:malformed)
        try Data("{".utf8).write(to:root.appendingPathComponent("MalformedJSON.wav.json"))
        let scanned = try entries(root)
        XCTAssertTrue(scanned.allSatisfy { $0["status"] as? String != "complete" })
        XCTAssertEqual(scanned.first { $0["file"] as? String == "MalformedEvidence.wav" }?["status"] as? String,"unverified")
        XCTAssertEqual(scanned.first { $0["file"] as? String == "MalformedJSON.wav" }?["status"] as? String,"invalid")
    }
    func testLargeAnalyzerMetadataIsAcceptedWithoutRetainingItsPayload() throws {
        let root = try folder(), name = "Analyzed.wav"
        var m = metadata(name)
        m.removeValue(forKey:"recordingSchemaVersion")
        m["analyzer"] = ["largeAnalysis":String(repeating:"x",count:128*1024)]
        m["anotherLargeField"] = String(repeating:"y",count:40*1024)
        try write(root,name,metadata:m)
        let entry = try XCTUnwrap(entries(root).first)
        XCTAssertEqual(entry["status"] as? String,"unverified")
        XCTAssertEqual(entry["metadataOmittedKeys"] as? [String],["analyzer","anotherLargeField"])
        let retained = try XCTUnwrap(entry["metadata"] as? [String:Any])
        XCTAssertNil(retained["analyzer"])
        XCTAssertEqual(retained["trackID"] as? String,"spotify-track-123")
        XCTAssertLessThanOrEqual(try JSONSerialization.data(withJSONObject:retained).count,RecordingCatalog.retainedMetadataLimit)
    }
    func testPartialShortLateStartAndDiscontinuousCapturesRemainIncomplete() throws {
        let root = try folder()
        let names = ["Active.partial.wav","Short.wav","Late.wav","Gap.wav"]
        for name in names {
            var m = metadata(name)
            if name == "Short.wav" { m["frames"] = 8000 }
            if name == "Late.wav" { m["observedStartPositionSeconds"] = 4 }
            if name == "Gap.wav" { m["coverageContinuous"] = false }
            try write(root,name,metadata:m,frames:name == "Short.wav" ? 8000 : 80000)
        }
        XCTAssertTrue(try entries(root,verify:true).allSatisfy { $0["status"] as? String == "incomplete" })
    }
    func testMissingTruncatedAndMismatchedWAVAreInvalid() throws {
        let root = try folder()
        try writeMetadata(root,"Missing.wav",metadata("Missing.wav"))
        try write(root,"Truncated.wav",metadata:metadata("Truncated.wav"))
        let truncated = root.appendingPathComponent("Truncated.wav")
        try Data(contentsOf:truncated).dropLast(8).write(to:truncated)
        var mismatch = metadata("Mismatch.wav"); mismatch["sampleRate"] = 44100
        try write(root,"Mismatch.wav",metadata:mismatch)
        let entries = try entries(root,verify:true)
        XCTAssertTrue(entries.allSatisfy { $0["status"] as? String == "invalid" })
        XCTAssertTrue(entries.allSatisfy { $0["audioVerified"] as? Bool == false })
        XCTAssertEqual(entries.first { $0["file"] as? String == "Missing.wav" }?["audioPresent"] as? Bool,false)
    }
    func testOrphansDuplicateTrackIDsAndOrderingArePreserved() throws {
        let root = try folder()
        for name in ["B.wav","A.wav"] { try write(root,name,metadata:metadata(name)) }
        try write(root,"Orphan.wav",metadata:nil)
        try write(root,"Active.partial.wav",metadata:nil)
        try Data("{}".utf8).write(to:root.appendingPathComponent("diagnostics.json"))
        let entries = try entries(root)
        XCTAssertEqual(entries.compactMap { $0["file"] as? String },["A.wav","Active.partial.wav","B.wav","Orphan.wav"])
        XCTAssertEqual(entries.filter { $0["trackID"] as? String == "spotify-track-123" }.count,2)
        XCTAssertEqual(entries.last?["status"] as? String,"unverified")
    }
    func testUnsafeReferencesSymlinksAndOversizedMetadataAreRejected() throws {
        let root = try folder()
        try write(root,"Reference.wav",metadata:metadata("../Outside.wav"))
        try FileManager.default.createSymbolicLink(at:root.appendingPathComponent("Link.wav"),withDestinationURL:root.appendingPathComponent("Reference.wav"))
        try writeMetadata(root,"Link.wav",metadata("Link.wav"))
        try FileManager.default.createSymbolicLink(at:root.appendingPathComponent("Sidecar.wav.json"),withDestinationURL:root.appendingPathComponent("Reference.wav.json"))
        let huge = root.appendingPathComponent("Huge.wav.json")
        try Data().write(to:huge)
        let handle = try FileHandle(forWritingTo:huge)
        try handle.truncate(atOffset:UInt64(RecordingCatalog.metadataLimit+1)); try handle.close()
        let scanned = try RecordingCatalog.scan(directory:root)
        XCTAssertTrue((scanned["entries"] as? [[String:Any]] ?? []).allSatisfy { $0["status"] as? String == "invalid" })
        XCTAssertEqual((scanned["issues"] as? [[String:String]])?.count,4)
    }
}
