#!/usr/bin/env python3
"""Explicit, checksum-pinned acquisition tool. The Rust application never downloads models.
Python is only needed for this development/install helper, not analysis or rendering.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.request

REVISION = '1ae768e78f1ad83b0ed3886241dc29ffde853c40'
BASE = f'https://raw.githubusercontent.com/danigb/beat-this-rs/{REVISION}/'
ASSETS = [
    ('mel_spectrogram', 'mel_spectrogram.onnx', 'fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9'),
    ('beat_this', 'beat_this_small.onnx', 'a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f'),
]


def atomic_json(path, record):
    fd, name = tempfile.mkstemp(prefix='.install-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as out:
            json.dump(record, out, indent=2, allow_nan=False)
            out.flush()
            os.fsync(out.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        Path(name).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--library', type=Path, required=True)
    args = parser.parse_args()
    root = args.library.resolve(strict=True)
    library = json.loads((root / 'library.json').read_text())
    if library['schema_version'] != 1:
        parser.error('unsupported library schema')
    for path in [root / 'locks', root / 'models', root / 'models/manifest.json', root / 'locks/writer.lock.json']:
        if path.is_symlink():
            parser.error(f'symlink metadata path: {path}')
    with (root / 'locks/writer.lock.json').open('a+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest_path = root / 'models/manifest.json'
        manifest = json.loads(manifest_path.read_text())
        if manifest['schema_version'] != 1:
            parser.error('unsupported model manifest schema')
        entries = []
        for model_id, filename, checksum in ASSETS:
            target = root / 'models' / filename
            partial = target.with_suffix('.download')
            if target.is_symlink() or partial.is_symlink():
                parser.error('symlink model path')
            valid = target.exists() and hashlib.sha256(target.read_bytes()).hexdigest() == checksum
            if not valid:
                size = partial.stat().st_size if partial.exists() else 0
                request = urllib.request.Request(BASE + 'models/' + filename)
                if size:
                    request.add_header('Range', f'bytes={size}-')
                with urllib.request.urlopen(request, timeout=60) as response:
                    append = size > 0 and response.status == 206
                    if append and not response.headers.get('Content-Range', '').startswith(f'bytes {size}-'):
                        raise ValueError('server returned incorrect resume range')
                    with partial.open('ab' if append else 'wb') as out:
                        while chunk := response.read(1024 * 1024):
                            if out.tell() + len(chunk) > 512 * 1024 * 1024:
                                raise ValueError('model exceeds size limit')
                            out.write(chunk)
                        out.flush()
                        os.fsync(out.fileno())
                if hashlib.sha256(partial.read_bytes()).hexdigest() != checksum:
                    partial.unlink()
                    raise ValueError(f'checksum mismatch: {filename}')
                os.replace(partial, target)
            entries.append({
                'model_id': model_id,
                'file': {'path': f'models/{filename}', 'hash': {'algorithm': 'sha256', 'digest': checksum}},
                'upstream_url': BASE + 'models/' + filename,
                'revision': REVISION,
                'code_license': 'MIT', 'weight_license': 'MIT (Beat This! small1 converted asset)',
                'attribution': 'CPJKU Beat This! authors; danigb/beat-this-rs conversion and streaming port',
                'redistribution': 'Retain upstream MIT notices; reference-parity and collection evaluation pending',
            })
        preserved = [m for m in manifest['models'] if m['model_id'] not in {a[0] for a in ASSETS}]
        manifest['models'] = preserved + entries
        atomic_json(manifest_path, manifest)
        print(json.dumps({'installed': [e['file'] for e in entries], 'library': str(root)}))


if __name__ == '__main__':
    main()
