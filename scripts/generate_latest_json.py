#!/usr/bin/env python3

import json
import sys
from pathlib import Path


def asset_entry(release_dir: Path, base_url: str, filename: str):
    return {
        "url": f"{base_url}/{filename}",
        "signature": (release_dir / f"{filename}.minisig")
        .read_text(encoding="utf-8")
        .strip(),
    }


def file_exists(release_dir: Path, filename: str) -> bool:
    return (release_dir / filename).is_file() and (
        release_dir / f"{filename}.minisig"
    ).is_file()


def add_linux_musl_platform(
    manifest: dict,
    release_dir: Path,
    base_url: str,
    platform_key: str,
    musl_name: str,
):
    if file_exists(release_dir, musl_name):
        manifest["platforms"][platform_key] = asset_entry(
            release_dir, base_url, musl_name
        )


def main() -> int:
    if len(sys.argv) != 6:
        print(
            "Usage: generate_latest_json.py <release_dir> <version> <pub_date> <base_url> <notes>",
            file=sys.stderr,
        )
        return 1

    release_dir = Path(sys.argv[1]).resolve()
    version = sys.argv[2]
    pub_date = sys.argv[3]
    base_url = sys.argv[4].rstrip("/")
    notes = sys.argv[5]

    manifest = {
        "version": version,
        "notes": notes,
        "pub_date": pub_date,
        "platforms": {},
    }

    # This fork only publishes static Linux musl builds.
    add_linux_musl_platform(
        manifest,
        release_dir,
        base_url,
        "linux-x86_64",
        "cc-switch-cli-linux-x64-musl.tar.gz",
    )
    add_linux_musl_platform(
        manifest,
        release_dir,
        base_url,
        "linux-aarch64",
        "cc-switch-cli-linux-arm64-musl.tar.gz",
    )

    if not manifest["platforms"]:
        print("No signed release assets found to build latest.json", file=sys.stderr)
        return 1

    output_path = release_dir / "latest.json"
    output_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
