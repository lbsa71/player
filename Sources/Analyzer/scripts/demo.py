#!/usr/bin/env python3
"""Generate original CC0 signals and exercise the CLI. Never modifies user audio."""
import argparse
import json
import math
from pathlib import Path
import struct
import subprocess
import sys
import wave


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--neural', action='store_true')
    args = parser.parse_args()
    base = Path(__file__).resolve().parents[1]
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error('choose a new or empty output directory')
    output.mkdir(parents=True, exist_ok=True)
    music = output / 'music'
    music.mkdir()
    for name, pitch, pulse in [('steady.wav', 220., True), ('drifting.wav', 330., True), ('free-time.wav', 275., False)]:
        rate = 48000
        duration = 20
        beats, t = [], 0.25
        while t < duration:
            beats.append(t)
            t += 0.5 if name == 'steady.wav' else 0.5 + 0.015 * math.sin(t / 4)
        with wave.open(str(music / name), 'wb') as f:
            f.setparams((2, 2, rate, 0, 'NONE', 'not compressed'))
            block = bytearray()
            beat = 0
            for i in range(rate * duration):
                t = i / rate
                while beat + 1 < len(beats) and beats[beat + 1] <= t:
                    beat += 1
                dt = t - beats[beat]
                pluck = math.exp(-max(dt, 0) * 7) if dt >= 0 and pulse else 0.15 * (1 + math.sin(t * 1.7))
                x = 0.18 * pluck * (math.sin(math.tau * pitch * t) + 0.3 * math.sin(math.tau * pitch * 2 * t))
                if pulse and dt >= 0:
                    x += (0.25 if beat % 4 == 0 else 0.14) * math.sin(math.tau * 70 * dt) * math.exp(-dt * 60)
                sample = int(x * 32767)
                block.extend(struct.pack('<hh', sample, sample))
                if len(block) >= 65536:
                    f.writeframes(block)
                    block.clear()
            f.writeframes(block)
    subprocess.run(['cargo', 'build', '--locked', '--release', '--bin', 'mixlib'], cwd=base, check=True)
    binary = base / 'target/release/mixlib'
    library = output / 'library'

    def run(*arguments):
        return json.loads(subprocess.check_output([str(binary), '--library', str(library), '--json', *arguments]))

    run('init')
    run('scan', '--source', str(music))
    if args.neural:
        subprocess.run([sys.executable, str(base / 'scripts/install-models.py'), '--library', str(library)], check=True)
    analyze = ['analyze', '--resume'] + ([] if args.neural else ['--dsp-only'])
    first_analysis = run(*analyze)
    if first_analysis['counts'].get('failed', 0):
        raise RuntimeError(first_analysis)
    tracks = run('inspect')
    by_name = {Path(t['source_locations'][0]['relative_path']).name: t['track_id'] for t in tracks}
    plan = run('plan', '--all', '--seed', '42', '--start', by_name['steady.wav'], '--end', by_name['free-time.wav'])
    (output / 'plan.json').write_text(json.dumps(plan, indent=2) + '\n')
    run('preview', '--plan', plan['plan_id'])
    render = run('render', '--plan', plan['plan_id'], '--output', str(output / 'renders/mix.wav'))
    before = {t['track_id']: t['active_analysis'] for t in run('inspect')}
    scan = run('scan')
    repeat = run(*analyze)
    after = {t['track_id']: t['active_analysis'] for t in run('inspect')}
    assert before == after and repeat['counts']['unchanged'] == 3
    validation = run('validate')
    report = {'schema_version': 1, 'report_id': 'demo', 'extensions': {}, 'scan': scan, 'repeat_analysis': repeat, 'render': render, 'validation': validation, 'output': str(output)}
    (output / 'demo-report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
