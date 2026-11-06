#!/usr/bin/env python3
"""Protocol smoke test. Requires the installed Player app to be open; never starts audio."""
import json
import pathlib
import select
import subprocess
import sys

binary = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path.home() / "Applications/Player.app/Contents/MacOS/PlayerMCP"
process = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)

def read_response():
    assert select.select([process.stdout], [], [], 8)[0], "MCP response timed out"
    return json.loads(process.stdout.readline())

def request(identifier, method, params=None):
    message = {"jsonrpc": "2.0", "id": identifier, "method": method}
    if params is not None:
        message["params"] = params
    process.stdin.write(json.dumps(message) + "\n")
    process.stdin.flush()
    response = read_response()
    assert response["id"] == identifier
    return response

try:
    initialized = request(1, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "player-test", "version": "1"}})
    assert initialized["result"]["protocolVersion"] == "2024-11-05"
    process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
    process.stdin.flush()
    tools = request(2, "tools/list")["result"]["tools"]
    assert {tool["name"] for tool in tools} == {"player_status", "player_devices", "player_logs", "player_start", "player_stop", "player_volume", "player_login", "player_spotify_devices", "player_select_spotify_device", "player_transport", "player_quit", "player_record_start", "player_record_stop", "bridge_status", "bridge_devices", "bridge_start", "bridge_stop", "bridge_monitor", "bridge_monitor_stop", "bridge_gain", "bridge_record_start", "bridge_record_stop", "bridge_record_track", "bridge_use_default", "bridge_restore_default"}
    assert request(3, "ping")["result"] == {}
    assert request(4, "not-a-method")["error"]["code"] == -32601
    process.stdin.write("not json\n")
    process.stdin.flush()
    assert read_response()["error"]["code"] == -32700
    for identifier, tool in enumerate(["player_status", "player_devices", "player_logs"], 10):
        result = request(identifier, "tools/call", {"name": tool, "arguments": {}})["result"]
        assert result["isError"] is False, result
        assert json.loads(result["content"][0]["text"])["ok"] is True
    invalid = request(20, "tools/call", {"name": "player_volume", "arguments": {"gain": 2}})["result"]
    assert invalid["isError"] is True
    print("MCP handshake, discovery, notifications, ping, protocol errors, live status/devices/logs, and invalid gain: PASS")
finally:
    process.stdin.close()
    process.wait(timeout=5)
