#!/usr/bin/env python3
"""
Atomically imports an OCI Image Layout directory (produced by `skopeo copy ... oci:<dir>:<tag>`)
into the PITS Talos image cache directory (`/var/lib/pits/tinkerbell-20261108/image-cache-v1.14.5`)
served by `tink-system/talos-image-cache` at `http://10.89.20.14:3172`.
"""

import hashlib
import json
import os
import shutil
import sys
from pathlib import Path


def atomic_copy(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.with_name(f".{dst.name}.tmp.{os.getpid()}")
    shutil.copyfile(src, tmp)
    os.chmod(tmp, 0o644)
    os.replace(tmp, dst)


def atomic_write_bytes(data: bytes, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.with_name(f".{dst.name}.tmp.{os.getpid()}")
    tmp.write_bytes(data)
    os.chmod(tmp, 0o644)
    os.replace(tmp, dst)


def resolve_single_manifest(oci_dir: Path) -> tuple[bytes, str]:
    index_data = json.loads((oci_dir / "index.json").read_text(encoding="utf-8"))
    manifests = index_data.get("manifests") or []
    if not manifests:
        raise RuntimeError(f"No manifests found in {oci_dir}/index.json")

    desc = manifests[0]
    digest = desc["digest"]  # sha256:<hex>
    algo, hex_hash = digest.split(":", 1)
    blob_path = oci_dir / "blobs" / algo / hex_hash
    raw = blob_path.read_bytes()
    parsed = json.loads(raw.decode("utf-8"))

    # If nested OCI image index, pick linux/amd64 manifest
    if parsed.get("manifests"):
        chosen = None
        for m in parsed["manifests"]:
            plat = m.get("platform") or {}
            if plat.get("os") == "linux" and plat.get("architecture") == "amd64":
                chosen = m
                break
        if chosen is None:
            chosen = parsed["manifests"][0]
        algo, hex_hash = chosen["digest"].split(":", 1)
        raw = (oci_dir / "blobs" / algo / hex_hash).read_bytes()

    computed = "sha256:" + hashlib.sha256(raw).hexdigest()
    return raw, computed


def import_oci(oci_dir: Path, cache_root: Path, image_ref: str) -> dict:
    # Example image_ref: "docker.io/pits/test-runner:v1" or "registry.k8s.io/agent-sandbox/agent-sandbox-controller:v1.0.11"
    if ":" not in image_ref.split("/")[-1]:
        raise ValueError(f"image_ref must include explicit :tag, got {image_ref}")
    repo_part, tag = image_ref.rsplit(":", 1)

    manifest_bytes, manifest_digest = resolve_single_manifest(oci_dir)
    manifest_obj = json.loads(manifest_bytes.decode("utf-8"))

    blob_dir = cache_root / "blob"
    blob_dir.mkdir(parents=True, exist_ok=True)

    # Copy config + all layers into blob/sha256-<hex>
    descriptors = [manifest_obj["config"]] + list(manifest_obj.get("layers", []))
    copied_blobs = 0
    for d in descriptors:
        algo, hex_hash = d["digest"].split(":", 1)
        src_blob = oci_dir / "blobs" / algo / hex_hash
        if not src_blob.is_file():
            raise FileNotFoundError(f"Missing OCI blob {src_blob}")
        dst_blob = blob_dir / f"{algo}-{hex_hash}"
        if not dst_blob.exists() or dst_blob.stat().st_size != src_blob.stat().st_size:
            atomic_copy(src_blob, dst_blob)
            copied_blobs += 1

    # Also store the manifest itself in blob/ in case registry clients query by blob
    m_algo, m_hex = manifest_digest.split(":", 1)
    atomic_write_bytes(manifest_bytes, blob_dir / f"{m_algo}-{m_hex}")

    manifest_base = cache_root / "manifests" / repo_part
    atomic_write_bytes(manifest_bytes, manifest_base / "reference" / tag)
    atomic_write_bytes(manifest_bytes, manifest_base / "digest" / f"{m_algo}-{m_hex}")

    return {
        "image": f"{repo_part}:{tag}",
        "digest": manifest_digest,
        "layers": len(manifest_obj.get("layers", [])),
        "copiedBlobs": copied_blobs,
    }


def main() -> None:
    if len(sys.argv) != 4:
        print("Usage: import-to-talos-cache.py <oci-layout-dir> <talos-image-cache-dir> <full-image-ref:tag>", file=sys.stderr)
        sys.exit(2)

    oci_dir = Path(sys.argv[1])
    cache_root = Path(sys.argv[2])
    image_ref = sys.argv[3]

    info = import_oci(oci_dir, cache_root, image_ref)
    print(json.dumps(info))


if __name__ == "__main__":
    main()
