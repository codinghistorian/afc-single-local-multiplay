#!/usr/bin/env python3
"""Run the release dependency audit with a verified Steamworks backport."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
STEAMWORKS_ADVISORY = "RUSTSEC-2026-0121"
UNMAINTAINED_TRANSITIVE_ADVISORIES = (
    "RUSTSEC-2025-0141",  # bincode 2, via Lightyear
    "RUSTSEC-2024-0436",  # paste, via Bevy/Lightyear/Steamworks
    "RUSTSEC-2025-0134",  # rustls-pemfile, via wtransport
    "RUSTSEC-2026-0192",  # ttf-parser, via Bevy's text stack
)


def fail(message: str) -> None:
    print(f"dependency audit preflight failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def verify_steamworks_backport() -> None:
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    try:
        patch = manifest["patch"]["crates-io"]["steamworks"]
        patch_path = patch["path"]
    except (KeyError, TypeError) as error:
        fail(f"Steamworks is not supplied by a local crates.io patch ({error})")

    if patch_path != "vendor/steamworks":
        fail(f"unexpected Steamworks patch path: {patch_path!r}")

    vendor_root = ROOT / patch_path
    vendor_manifest = tomllib.loads(
        (vendor_root / "Cargo.toml").read_text(encoding="utf-8")
    )
    if vendor_manifest.get("package", {}).get("version") != "0.12.2":
        fail("the audited exception only applies to vendored Steamworks 0.12.2")

    user_source = (vendor_root / "src" / "user.rs").read_text(encoding="utf-8")
    required_fragments = (
        "k_EAuthSessionResponseAuthTicketNetworkIdentityFailure => {",
        "Err(AuthSessionValidateError::AuthTicketNetworkIdentityFailure)",
        "AuthTicketNetworkIdentityFailure,",
    )
    missing = [fragment for fragment in required_fragments if fragment not in user_source]
    if missing:
        fail(
            "the RUSTSEC-2026-0121 callback backport is incomplete; missing "
            + ", ".join(repr(fragment) for fragment in missing)
        )

    print(
        f"verified {STEAMWORKS_ADVISORY} fix in the vendored Steamworks callback"
    )


def main() -> int:
    verify_steamworks_backport()
    if shutil.which("cargo-audit") is None:
        fail("cargo-audit is required (install cargo-audit 0.22.1)")

    command = [
        "cargo",
        "audit",
        "--quiet",
        "--ignore",
        STEAMWORKS_ADVISORY,
    ]
    for advisory in UNMAINTAINED_TRANSITIVE_ADVISORIES:
        command.extend(("--ignore", advisory))
    command.extend(("--deny", "warnings"))
    print(
        "allowing known unmaintained transitive dependencies only: "
        + ", ".join(UNMAINTAINED_TRANSITIVE_ADVISORIES),
        flush=True,
    )
    return subprocess.run(command, cwd=ROOT, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
