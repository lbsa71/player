#!/usr/bin/env python3
"""Record an endpoint-only synthetic signal, with monitoring off. Needs installed driver/input permission."""
import array
import json
import pathlib
import struct
import subprocess
import time

root = pathlib.Path(__file__).resolve().parent.parent
agent = pathlib.Path.home() / 'Applications/AudioBridge.app/Contents/MacOS/AudioBridge'

def call(method, arguments=None):
    result = json.loads(subprocess.check_output([str(agent), '--call', method, json.dumps(arguments or {})]))
    assert result.get('ok'), result
    return result

def settled():
    for _ in range(50):
        state = call('bridge_status')
        if not state['busy']:
            assert not state['controlError'], state
            return state
        time.sleep(0.1)
    raise RuntimeError('Input permission or startup is pending; approve macOS prompt and rerun.')

before = call('bridge_status')
assert before['installed'], 'Driver is not loaded'
assert not before['recording']['enabled'], 'Stop the existing recording first'
assert not before['monitoring'], 'Stop monitoring for this silent synthetic check'
call('bridge_start'); settled()
label = 'Endpoint exact samples ' + time.strftime('%Y%m%d-%H%M%S')
call('bridge_record_start', dict(album='Audio Bridge Test', artist='Synthetic', track=label))
assert settled()['recording']['enabled']
try:
    subprocess.run([str(root/'build/bridge-test-tone'), '5'], check=True)
    # Confirm that listener gain changes cannot alter the recording path.
    call('bridge_gain', dict(gain=0))
    subprocess.run([str(root/'build/bridge-test-tone'), '3'], check=True)
finally:
    call('bridge_record_stop')
    call('bridge_gain', dict(gain=before['monitorGain']))
for _ in range(100):
    state = call('bridge_status')
    if not state['recording']['finishing']:
        break
    time.sleep(0.05)
assert not state['recording']['error'], state
path = pathlib.Path(state['recording']['file'])
data = path.read_bytes()
assert data[:4] == b'RIFF' and data[8:12] == b'WAVE'
assert struct.unpack_from('<I', data, 4)[0] == len(data)-8
offset = 12
samples = None
while offset+8 <= len(data):
    tag, size = struct.unpack_from('<4sI', data, offset)
    chunk = data[offset+8:offset+8+size]
    if tag == b'fmt ':
        encoding, channels, rate = struct.unpack_from('<HHI', chunk)
        assert (encoding, channels) == (3, 2)
    elif tag == b'data':
        samples = array.array('f'); samples.frombytes(chunk)
    offset += 8+size+(size%2)
assert samples is not None
signal = sum(x != 0 for x in samples[::2])
assert signal > rate*6, (signal, rate)
assert all(l == -r for l,r in zip(samples[::2],samples[1::2]))
assert all(-498 <= x*32768 <= 498 and x*32768 == int(x*32768) for x in samples)
# Both output queue runs begin at the same deterministic sample; allow their
# silent boundary. Report every discontinuity inside each non-silent run.
discontinuities = 0
previous = None
silent = 0
for sample in samples[::2]:
    value = round(sample*32768)
    if value == 0:
        silent += 1
        continue
    if previous is not None and silent < 2:
        expected = (previous+498+1+silent)%997-498
        discontinuities += value != expected
    previous, silent = value, 0
result = dict(file=str(path), frames=len(samples)//2, sampleRate=rate,
    signalFrames=signal, discontinuities=discontinuities,
    bitExactSequence=discontinuities == 0, status=state)
(root/'build/bridge-live-validation.json').write_text(json.dumps(result, indent=2))
print(json.dumps({k:v for k,v in result.items() if k != 'status'}, indent=2))
assert discontinuities == 0
assert state['invalidBuffers'] == before['invalidBuffers']
