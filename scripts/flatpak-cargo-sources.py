#!/usr/bin/env python3
import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"
CARGO_CONFIG = """[source.vendored-sources]
directory = "/run/build/tokenburntrack/cargo/vendor"

[source.crates-io]
replace-with = "vendored-sources"
"""


def main() -> int:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    sources = []
    for pkg in lock["package"]:
        source = pkg.get("source")
        if source is None:
            continue
        if source != CRATES_IO:
            print(f"unsupported source for {pkg['name']}: {source}", file=sys.stderr)
            return 1
        name, version, checksum = pkg["name"], pkg["version"], pkg["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        sources.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
    sources.append(
        {
            "type": "inline",
            "contents": CARGO_CONFIG,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )
    out = ROOT / "packaging/flatpak/cargo-sources.json"
    out.write_text(json.dumps(sources, indent=2) + "\n")
    print(f"wrote {len(sources)} sources to {out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
