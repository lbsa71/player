#!/usr/bin/env python3
"""Register a checksum-pinned local HTDemucs checkpoint without redownloading it."""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil

NAME = "955717e8-8726e21a.th"
CHECKSUM = "8726e21a993978c7ba086d3872e7608d7d5bfca646ca4aca459ffda844faa8b4"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--checkpoint", type=Path, required=True)
    args = parser.parse_args()
    root = args.library.resolve(strict=True)
    checkpoint = args.checkpoint.resolve(strict=True)
    digest = hashlib.sha256()
    with checkpoint.open("rb") as source:
        for block in iter(lambda: source.read(1024*1024), b""):
            digest.update(block)
    if digest.hexdigest() != CHECKSUM:
        parser.error("HTDemucs checkpoint checksum mismatch")
    target = root/"models"/NAME
    partial = target.with_suffix(".installing")
    manifest_path = root/"models/manifest.json"
    for path in [root/"locks",root/"models",target,partial,manifest_path,root/"locks/writer.lock.json"]:
        if path.is_symlink():
            parser.error(f"symlink metadata path: {path}")
    spec = importlib.util.spec_from_file_location("model_install",Path(__file__).with_name("install-models.py"))
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)
    with (root/"locks/writer.lock.json").open("a+") as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        manifest = json.loads(manifest_path.read_text())
        if manifest["schema_version"] != 1:
            parser.error("unsupported model manifest schema")
        if checkpoint != target:
            shutil.copyfile(checkpoint,partial)
            with partial.open("rb") as source:
                os.fsync(source.fileno())
            os.replace(partial,target)
        model = {"model_id":"stem_htdemucs","file":{"path":f"models/{NAME}",
                 "hash":{"algorithm":"sha256","digest":CHECKSUM}},
                 "upstream_url":f"https://dl.fbaipublicfiles.com/demucs/hybrid_transformer/{NAME}",
                 "revision":"demucs-4.0.1-955717e8","code_license":"MIT",
                 "weight_license":"MIT (Demucs distribution)","attribution":"Meta Demucs authors",
                 "redistribution":"Retain upstream MIT notices"}
        manifest["models"] = [m for m in manifest["models"] if m["model_id"]!=model["model_id"]]+[model]
        installer.atomic_json(manifest_path,manifest)
        print(json.dumps({"installed":model["file"],"library":str(root)}))


if __name__ == "__main__":
    main()
