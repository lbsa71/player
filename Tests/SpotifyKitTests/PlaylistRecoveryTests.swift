import XCTest
@testable import SpotifyKit

final class PlaylistRecoveryTests: XCTestCase {
    private let playlist = "spotify:playlist:example"
    private func started() -> PlaylistRecovery {
        var recovery = PlaylistRecovery()
        recovery.start(playlistURI:playlist, deviceID:"mac", now:0)
        return recovery
    }
    private func observation(_ track:String = "first", _ position:Double = 0,
                             context:String? = "spotify:playlist:example", device:String? = "mac",
                             playing:Bool = true, shuffle:Bool? = false, repeatMode:String? = "off",
                             stale:Bool = false) -> PlaylistRecovery.Observation {
        .init(contextURI:context, deviceID:device, trackID:track, positionSeconds:position,
              isPlaying:playing, shuffle:shuffle, repeatMode:repeatMode, stale:stale)
    }
    private func assertStop(_ action: PlaylistRecovery.Action, file:StaticString = #filePath, line:UInt = #line) {
        guard case .stop = action else { return XCTFail("Expected stop, got \(action)",file:file,line:line) }
    }

    func testIncompleteTrackCapturesAndCatalogRefreshDoesNotInterruptVisit() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation(),completedTrackIDs:[],now:0),.capture)
        XCTAssertTrue(recovery.isCapturing)
        XCTAssertEqual(recovery.observe(observation("first",5),completedTrackIDs:["first"],now:5),.capture)
        XCTAssertEqual(recovery.observe(observation("second",1),completedTrackIDs:["first"],now:6),.capture)
    }
    func testCompletedTrackSkipsOnceAndWaitsForDifferentTrack() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation(),completedTrackIDs:["first"],now:0),.skip)
        XCTAssertFalse(recovery.isCapturing)
        XCTAssertEqual(recovery.observe(observation("first",1),completedTrackIDs:["first"],now:1),.wait)
        XCTAssertEqual(recovery.observe(observation("second",0.5),completedTrackIDs:["first"],now:2),.capture)
    }
    func testLateIncompleteTrackRestartsOnceAndWaitsForObservedSeek() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation("first",15),completedTrackIDs:[],now:0),.restart)
        XCTAssertEqual(recovery.observe(observation("first",16),completedTrackIDs:[],now:1),.wait)
        XCTAssertFalse(recovery.isCapturing)
        XCTAssertEqual(recovery.observe(observation("first",0.8),completedTrackIDs:[],now:2),.capture)
        XCTAssertEqual(recovery.observe(observation("first",3),completedTrackIDs:[],now:4),.capture)
    }
    func testUnconfirmedSkipTimesOutWithoutRepeatingCommand() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation(),completedTrackIDs:["first"],now:0),.skip)
        for second in 1...9 {
            XCTAssertEqual(recovery.observe(observation("first",Double(second)),completedTrackIDs:["first"],now:Double(second)),.wait)
        }
        assertStop(recovery.observe(observation("first",10),completedTrackIDs:["first"],now:10))
        XCTAssertFalse(recovery.isActive)
        assertStop(recovery.observe(observation("second",0),completedTrackIDs:["first"],now:11))
    }
    func testSameIDDuplicateRequiresProgressResetAndCannotReconfirmSameSample() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation("first",20),completedTrackIDs:["first"],now:0),.skip)
        XCTAssertEqual(recovery.observe(observation("first",0.2),completedTrackIDs:["first"],now:1),.skip)
        XCTAssertEqual(recovery.observe(observation("first",0.2),completedTrackIDs:["first"],now:2),.wait)
        XCTAssertEqual(recovery.observe(observation("first",0.3),completedTrackIDs:["first"],now:3),.wait)
        assertStop(recovery.observe(observation("first",0.3),completedTrackIDs:["first"],now:11))
    }
    func testSameIDAtBeginningDoesNotProveSkipAndTimesOut() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation("first",0.4),completedTrackIDs:["first"],now:0),.skip)
        XCTAssertEqual(recovery.observe(observation("first",0),completedTrackIDs:["first"],now:1),.wait)
        assertStop(recovery.observe(observation("first",0),completedTrackIDs:["first"],now:10))
    }
    func testSameIDResetStartsNewVisitAndLateResetRequiresRestart() {
        var recovery = started()
        XCTAssertEqual(recovery.observe(observation(),completedTrackIDs:[],now:0),.capture)
        XCTAssertEqual(recovery.observe(observation("first",30),completedTrackIDs:[],now:1),.capture)
        XCTAssertEqual(recovery.observe(observation("first",0.4),completedTrackIDs:["first"],now:2),.skip)

        recovery = started()
        _ = recovery.observe(observation(),completedTrackIDs:[],now:0)
        _ = recovery.observe(observation("first",30),completedTrackIDs:[],now:1)
        XCTAssertEqual(recovery.observe(observation("first",8),completedTrackIDs:[],now:2),.restart)
    }
    func testTrackChangeBeforeRestartConfirmationStops() {
        var recovery = started()
        _ = recovery.observe(observation("first",8),completedTrackIDs:[],now:0)
        assertStop(recovery.observe(observation("second",0),completedTrackIDs:[],now:1))
        XCTAssertFalse(recovery.isCapturing)
    }
    func testUnsafePlaybackChangesCancelRecovery() {
        let unsafe = [observation(context:"spotify:playlist:other"), observation(device:"other"),
                      observation(playing:false), observation(shuffle:true), observation(repeatMode:"track"),
                      observation(stale:true), observation(context:nil), observation(shuffle:nil),
                      observation("",0), observation("first",Double.nan)]
        for input in unsafe {
            var recovery = started()
            _ = recovery.observe(observation(),completedTrackIDs:[],now:0)
            assertStop(recovery.observe(input,completedTrackIDs:[],now:1))
            XCTAssertFalse(recovery.isActive)
            XCTAssertFalse(recovery.isCapturing)
        }
    }
    func testUnavailablePlaybackAndClockRollbackStop() {
        var recovery = started()
        assertStop(recovery.observe(.init(playback:nil),completedTrackIDs:[],now:0))
        recovery = started()
        _ = recovery.observe(observation(),completedTrackIDs:[],now:2)
        assertStop(recovery.observe(observation(),completedTrackIDs:[],now:1))
    }
    func testCancellationClearsPendingAndExplicitStartCreatesFreshRun() {
        var recovery = started()
        _ = recovery.observe(observation(),completedTrackIDs:["first"],now:0)
        XCTAssertEqual(recovery.cancel(reason:"Command failed"),.stop("Command failed"))
        XCTAssertEqual(recovery.observe(observation("second",0),completedTrackIDs:[],now:1),.stop("Command failed"))
        recovery.start(playlistURI:playlist,deviceID:"mac",now:2)
        XCTAssertEqual(recovery.observe(observation("second",0),completedTrackIDs:[],now:2),.capture)
    }
}
