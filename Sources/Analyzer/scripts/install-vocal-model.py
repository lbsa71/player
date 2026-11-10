#!/usr/bin/env python3
"""Install the pinned local Whisper small model for lyric-boundary evidence."""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import urllib.request

CHECKSUM = "9ecf779972d90ba49c06d968637d720dd632c55bbf19d441fb42bf17a411e794"
URL = f"https://openaipublic.azureedge.net/main/whisper/models/{CHECKSUM}/small.pt"


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024*1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, required=True)
    args = parser.parse_args()
    root = args.library.resolve(strict=True)
    manifest_path = root / "models/manifest.json"
    target = root / "models/whisper-small.pt"
    partial = target.with_suffix(".download")
    for path in [root/"locks", root/"models", manifest_path, target, partial, root/"locks/writer.lock.json"]:
        if path.is_symlink():
            parser.error(f"symlink metadata path: {path}")
    spec = importlib.util.spec_from_file_location("model_install", Path(__file__).with_name("install-models.py"))
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)
    with (root/"locks/writer.lock.json").open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest = json.loads(manifest_path.read_text())
        if manifest["schema_version"] != 1:
            parser.error("unsupported model manifest schema")
        if not target.exists() or checksum(target) != CHECKSUM:
            size = partial.stat().st_size if partial.exists() else 0
            request = urllib.request.Request(URL)
            if size:
                request.add_header("Range", f"bytes={size}-")
            with urllib.request.urlopen(request, timeout=60) as response:
                append = size > 0 and response.status == 206
                if append and not response.headers.get("Content-Range", "").startswith(f"bytes {size}-"):
                    raise ValueError("incorrect download resume range")
                with partial.open("ab" if append else "wb") as output:
                    while block := response.read(1024*1024):
                        if output.tell() + len(block) > 512*1024*1024:
                            raise ValueError("vocal model exceeds size bound")
                        output.write(block)
                    output.flush()
                    os.fsync(output.fileno())
            if checksum(partial) != CHECKSUM:
                partial.unlink()
                raise ValueError("vocal model checksum mismatch")
            os.replace(partial, target)
        model = {"model_id": "vocal_phrase_whisper", "file": {"path": "models/whisper-small.pt",
                 "hash": {"algorithm": "sha256", "digest": CHECKSUM}},
                 "upstream_url": URL, "revision": "openai-whisper-20250625-small",
                 "code_license": "MIT", "weight_license": "MIT",
                 "attribution": "OpenAI Whisper authors",
                 "redistribution": "Retain upstream MIT license; singing phrase accuracy requires collection evaluation"}
        manifest["models"] = [m for m in manifest["models"] if m["model_id"] != model["model_id"]] + [model]
        installer.atomic_json(manifest_path, manifest)
        print(json.dumps({"installed": model["file"], "library": str(root)}))


if __name__ == "__main__":
    main()
