import Foundation
import PlayerControl

let toolDefinitions: [[String: Any]] = ([
    ["name": "player_status", "description": "Read route state, sample counters, format, source discovery and latest errors."],
    ["name": "player_devices", "description": "List local physical outputs, stable UIDs, formats and prototype support."],
    ["name": "player_logs", "description": "Read the bounded route event log. No audio or credentials appear in logs."],
    ["name": "player_start", "description": "Start Spotify-only capture on an explicitly selected output UID. May trigger macOS audio permission; returns accepted, then poll status.",
     "inputSchema": ["type": "object", "properties": ["outputUID": ["type": "string"]], "required": ["outputUID"], "additionalProperties": false]],
    ["name": "player_stop", "description": "Stop capture and restore ordinary Spotify output. Returns accepted, then poll status."],
    ["name": "player_record_start", "description": "Start per-track Float32 WAV recording of Player's rendered output in its configured debug folder. Requires active routing and fresh local track metadata. Track boundaries are approximate. Poll player_status.recording."],
    ["name": "player_record_stop", "description": "Stop WAV recording and finalize files without stopping playback. Poll player_status.recording until finishing=false."],
    ["name": "player_volume", "description": "Set local renderer gain from 0 to 1 without changing Spotify volume.",
     "inputSchema": ["type": "object", "properties": ["gain": ["type": "number", "minimum": 0, "maximum": 1]], "required": ["gain"], "additionalProperties": false]],
    ["name": "player_login", "description": "Open Spotify PKCE sign-in in the system browser. The user completes consent there. Never returns tokens."],
    ["name": "player_spotify_devices", "description": "Request a Spotify Connect device refresh. Returns accepted and current cached devices; poll player_status for refreshingDevices=false."],
    ["name": "player_select_spotify_device", "description": "Explicitly identify the Spotify Connect device on this Mac. The user must identify it; names alone are not proof of locality.",
     "inputSchema": ["type": "object", "properties": ["deviceID": ["type": "string"]], "required": ["deviceID"], "additionalProperties": false]],
    ["name": "player_transport", "description": "Queue a Spotify playback command. Poll player_status.spotify for the actual result. Do not automatically retry ambiguous next/previous failures.",
     "inputSchema": ["type": "object", "properties": ["action": ["type": "string", "enum": ["play", "pause", "next", "previous", "seek", "transfer", "playlist"]],
                       "playlist": ["type": "string"], "positionMs": ["type": "integer", "minimum": 0]], "required": ["action"], "additionalProperties": false]],
    ["name": "player_quit", "description": "Quit Player normally, releasing its audio route and local control socket. Ordinary Spotify output may resume."]
] + [
    ["name":"bridge_status","description":"Read generic endpoint capture, monitoring, recording, buffers and errors. Works without Player running."],
    ["name":"bridge_devices","description":"List physical monitoring outputs for Audio Bridge."],
    ["name":"bridge_start","description":"Start generic virtual-endpoint capture independently of physical monitoring. May request macOS input permission; poll bridge_status."],
    ["name":"bridge_stop","description":"Restore the previous default output when applicable, stop monitoring/capture, and finalize WAV recording."],
    ["name":"bridge_monitor","description":"Monitor Audio Bridge on a physical stereo output without changing or interrupting endpoint recording.",
     "inputSchema":["type":"object","properties":["outputUID":["type":"string"]],"required":["outputUID"],"additionalProperties":false]],
    ["name":"bridge_monitor_stop","description":"Stop physical listening while endpoint capture and recording continue."],
    ["name":"bridge_gain","description":"Change monitoring gain only. Does not affect endpoint WAV samples.",
     "inputSchema":["type":"object","properties":["gain":["type":"number","minimum":0,"maximum":1]],"required":["gain"],"additionalProperties":false]],
    ["name":"bridge_record_start","description":"Record raw endpoint input to Float32 WAVs. Optional labels are generic; no application dependency. Poll bridge_status for errors/finalization.",
     "inputSchema":["type":"object","properties":["playing":["type":"boolean"],"directory":["type":"string"],"album":["type":"string"],"artist":["type":"string"],"track":["type":"string"],"trackID":["type":"string"],"positionSeconds":["type":"number"],"leaseSeconds":["type":"number","minimum":2,"maximum":60]],"additionalProperties":false]],
    ["name":"bridge_record_track","description":"Supply observed track labels to the generic recorder, or playing=false to close the current segment. Refreshes a 10-second metadata lease.",
     "inputSchema":["type":"object","properties":["playing":["type":"boolean"],"album":["type":"string"],"artist":["type":"string"],"track":["type":"string"],"trackID":["type":"string"],"positionSeconds":["type":"number"]],"additionalProperties":false]],
    ["name":"bridge_record_stop","description":"Finalize endpoint WAV recording without stopping input capture or monitoring."],
    ["name":"bridge_use_default","description":"Explicitly route the Mac's regular default output to Audio Bridge. This affects all apps following that default, not only Spotify. Does not change the default microphone or alert output."],
    ["name":"bridge_restore_default","description":"Restore the physical default output remembered when Audio Bridge was explicitly selected."]
]).map { definition in
    var result = definition
    let name = result["name"] as! String
    if result["inputSchema"] == nil {
        result["inputSchema"] = ["type": "object", "properties": [:], "additionalProperties": false] as [String: Any]
    }
    result["annotations"] = ["readOnlyHint": ["player_status", "player_devices", "player_logs", "player_spotify_devices", "bridge_status", "bridge_devices"].contains(name),
                              "destructiveHint": false, "openWorldHint": name.hasPrefix("player_spotify") || ["player_login", "player_transport"].contains(name)]
    return result
}

func emit(_ value: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) else { return }
    data.append(10)
    FileHandle.standardOutput.write(data)
}

func call(_ name: String, _ arguments: [String: Any]) throws -> [String: Any] {
    guard toolDefinitions.contains(where: { $0["name"] as? String == name }) else {
        throw ControlError.message("Unknown Player tool: \(name)")
    }
    if name.hasPrefix("bridge_") { return try BridgeControl.request(name,arguments:arguments) }
    return try LocalControl.request(name, arguments: arguments)
}

if CommandLine.arguments.count > 1 {
    guard CommandLine.arguments.count >= 3, CommandLine.arguments[1] == "--call" else {
        FileHandle.standardError.write(Data("Usage: PlayerMCP [--call player_status|player_devices|player_logs|player_start|player_stop|player_volume '{JSON arguments}']\nWithout arguments, serves MCP JSON-RPC over stdio.\n".utf8))
        exit(2)
    }
    do {
        let data = Data((CommandLine.arguments.count > 3 ? CommandLine.arguments[3] : "{}").utf8)
        guard let arguments = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw ControlError.message("Arguments must be a JSON object.")
        }
        let result = try call(CommandLine.arguments[2], arguments)
        emit(result)
        exit(result["ok"] as? Bool == true ? 0 : 1)
    } catch { emit(["ok": false, "error": error.localizedDescription]); exit(1) }
}

while let line = readLine() {
    guard line.utf8.count <= 65_536 else {
        emit(["jsonrpc": "2.0", "id": NSNull(), "error": ["code": -32600, "message": "Request too large"]]); continue
    }
    do {
        guard let request = try JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any],
              request["jsonrpc"] as? String == "2.0", let method = request["method"] as? String else {
            emit(["jsonrpc": "2.0", "id": NSNull(), "error": ["code": -32600, "message": "Invalid request"]]); continue
        }
        guard let id = request["id"] else { continue } // Notifications receive no response.
        var response: [String: Any] = ["jsonrpc": "2.0", "id": id]
        switch method {
        case "initialize":
            response["result"] = ["protocolVersion": "2024-11-05", "capabilities": ["tools": ["listChanged": false]],
                                  "serverInfo": ["name": "player-local", "version": "0.4.0"]]
        case "ping": response["result"] = [:] as [String: Any]
        case "tools/list": response["result"] = ["tools": toolDefinitions]
        case "tools/call":
            do {
                guard let params = request["params"] as? [String: Any], let name = params["name"] as? String else {
                    throw ControlError.message("tools/call requires a tool name.")
                }
                let result = try call(name, params["arguments"] as? [String: Any] ?? [:])
                let encoded = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
                response["result"] = ["content": [["type": "text", "text": String(decoding: encoded, as: UTF8.self)]],
                                      "isError": result["ok"] as? Bool != true]
            } catch {
                response["result"] = ["content": [["type": "text", "text": error.localizedDescription]], "isError": true]
            }
        default: response["error"] = ["code": -32601, "message": "Method not found"]
        }
        emit(response)
    } catch {
        emit(["jsonrpc": "2.0", "id": NSNull(), "error": ["code": -32700, "message": "Parse error"]])
    }
}
