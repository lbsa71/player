#!/usr/bin/env python3
"""Regenerate inspectable dependency provenance from the tested application lockfile."""
import hashlib
import json
from pathlib import Path
import subprocess
import tomllib

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=root))
lock = tomllib.loads((root / 'Cargo.lock').read_text())
checksums = {(p['name'], p['version']): p.get('checksum') for p in lock['package']}
entries = []
for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if package['source'] is None:
        continue
    entries.append({
        'dependency_id': package['name'], 'version': package['version'],
        'upstream_url': package['repository'] or package['homepage'] or f"https://crates.io/crates/{package['name']}",
        'source': package['source'], 'sha256': checksums[(package['name'], package['version'])],
        'code_license': package['license'], 'attribution': package['authors'],
        'redistribution': 'Follow the declared SPDX license expression and upstream notices; see Cargo.lock for exact artifact',
    })
manifest = {'schema_version': 1, 'manifest_id': 'mixlib-dependencies', 'extensions': {}, 'dependencies': entries}
(root / 'dependency-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(f'{len(entries)} dependency entries; licenses missing: {[e["dependency_id"] for e in entries if not e["code_license"]]}')
