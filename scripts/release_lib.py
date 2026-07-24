"""Portable, fail-closed release packaging primitives for Animal Fighter Club.

The module intentionally uses only the Python standard library.  It never invokes
Cargo, SteamCMD, code-signing tools, or a network client.  Build orchestration is
owned by CI; this module audits, stages, seals, verifies, archives, and renders
preview-only SteamPipe configuration around already-built binaries.
"""

from __future__ import annotations

import hashlib
import json
import os
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import tempfile
import unicodedata
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterable, Mapping, Sequence


POLICY_SCHEMA_VERSION = 1
MANIFEST_SCHEMA_VERSION = 1
RELEASE_IDENTITY_SCHEMA_VERSION = 1
SPACEWAR_APP_ID = 480
MAX_JSON_BYTES = 4 * 1024 * 1024
MAX_IDENTITY_OUTPUT_BYTES = 64 * 1024
MAX_PORTABLE_PATH_BYTES = 240
MAX_RELEASE_LABEL_BYTES = 64
HEX_32_RE = re.compile(r"^[0-9a-f]{32}$")
HEX_64_RE = re.compile(r"^[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
RELEASE_LABEL_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$")
BUNDLE_ID_RE = re.compile(
    r"^[A-Za-z0-9][A-Za-z0-9-]*(?:\.[A-Za-z0-9][A-Za-z0-9-]*){2,}$"
)
APPLE_VERSION_RE = re.compile(r"^[0-9]+(?:\.[0-9]+){0,2}$")
PORTABLE_PATH_RE = re.compile(r"^[A-Za-z0-9._+ /-]+$")
CHECKSUM_LINE_RE = re.compile(r"^([0-9a-f]{64}) \*(.+)$")
PLACEHOLDER_RE = re.compile(r"@@([A-Z0-9_]+)@@")
WINDOWS_RESERVED_NAMES = {
    "aux",
    "con",
    "nul",
    "prn",
    *(f"com{index}" for index in range(1, 10)),
    *(f"lpt{index}" for index in range(1, 10)),
}

TOOL_SOURCE_PATHS = (
    ".github/workflows/ci.yml",
    ".github/workflows/cross-platform-determinism.yml",
    ".github/workflows/release-candidate.yml",
    "docs/release-packaging.md",
    "packaging/release-policy.json",
    "packaging/steam/app_build.vdf.in",
    "packaging/steam/depot_build.vdf.in",
    "scripts/release.py",
    "scripts/release_lib.py",
    "scripts/release_self_test.py",
)

LINUX_LAUNCHER = """#!/bin/sh
set -eu
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export LD_LIBRARY_PATH="$SELF_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$SELF_DIR/ffc-prototype" "$@"
"""

IDENTITY_KEYS = {
    "schema_version",
    "product_name",
    "package_name",
    "product_version",
    "release_label",
    "shipping",
    "steam_app_id",
    "steam_depot_build_id",
    "protocol_version",
    "simulation_version",
    "rng_scheme_version",
    "replay_format_version",
    "snapshot_schema_version",
    "compatibility_build_id",
    "gameplay_content_hash",
}

MANIFEST_KEYS = {
    "schema_version",
    "platform",
    "source_commit",
    "entrypoint",
    "release_identity",
    "steam",
    "payload",
}


class ReleaseError(RuntimeError):
    """Expected validation or packaging failure."""


def repository_root() -> Path:
    return Path(__file__).resolve().parent.parent


def default_policy_path(root: Path | None = None) -> Path:
    return (root or repository_root()) / "packaging" / "release-policy.json"


def _require_exact_keys(
    value: Mapping[str, Any], expected: set[str], context: str
) -> None:
    actual = set(value)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing or extra:
        details = []
        if missing:
            details.append(f"missing {missing}")
        if extra:
            details.append(f"unknown {extra}")
        raise ReleaseError(f"{context} has invalid fields: {', '.join(details)}")


def _is_plain_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _require_string(value: Any, context: str, *, nonempty: bool = True) -> str:
    if not isinstance(value, str) or (nonempty and not value):
        raise ReleaseError(f"{context} must be a non-empty string")
    return value


def _require_string_list(value: Any, context: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ReleaseError(f"{context} must be an array of strings")
    if len(value) != len(set(value)):
        raise ReleaseError(f"{context} contains duplicates")
    return list(value)


def load_json(path: Path, *, context: str = "JSON") -> Any:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise ReleaseError(f"could not inspect {context} {path}: {error}") from error
    if size <= 0 or size > MAX_JSON_BYTES:
        raise ReleaseError(f"{context} {path} has invalid size {size}")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ReleaseError(f"could not parse {context} {path}: {error}") from error


def deterministic_json_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n"
    ).encode("ascii")


def atomic_write_bytes(path: Path, contents: bytes, *, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.tmp-", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise


def load_policy(path: Path | None = None) -> dict[str, Any]:
    policy_path = (path or default_policy_path()).resolve()
    value = load_json(policy_path, context="release policy")
    if not isinstance(value, dict):
        raise ReleaseError("release policy root must be an object")
    expected = {
        "schema_version",
        "product_name",
        "package_name",
        "player_binary",
        "shipping_feature",
        "required_source_paths",
        "embedded_build_only_paths",
        "required_runtime_assets",
        "forbidden_basenames",
        "forbidden_suffixes",
        "platforms",
        "steam_linux_runtime",
    }
    _require_exact_keys(value, expected, "release policy")
    if value["schema_version"] != POLICY_SCHEMA_VERSION:
        raise ReleaseError(
            f"unsupported release policy schema {value['schema_version']!r}"
        )
    for key in ("product_name", "package_name", "player_binary", "shipping_feature"):
        _require_string(value[key], f"release policy {key}")
    for key in (
        "required_source_paths",
        "embedded_build_only_paths",
        "required_runtime_assets",
        "forbidden_basenames",
        "forbidden_suffixes",
    ):
        value[key] = _require_string_list(value[key], f"release policy {key}")

    for path_value in (
        value["required_source_paths"]
        + value["embedded_build_only_paths"]
        + value["required_runtime_assets"]
    ):
        portable_relative_path(path_value)

    if not isinstance(value["platforms"], dict):
        raise ReleaseError("release policy platforms must be an object")
    required_platforms = {"windows-x86_64", "linux-x86_64", "macos-universal"}
    _require_exact_keys(value["platforms"], required_platforms, "platform policy")
    platform_keys = {
        "binary_format",
        "binary_name",
        "entrypoint",
        "redistributable_name",
    }
    for platform_name, platform in value["platforms"].items():
        if not isinstance(platform, dict):
            raise ReleaseError(f"platform {platform_name} policy must be an object")
        _require_exact_keys(platform, platform_keys, f"platform {platform_name}")
        for key in platform_keys:
            _require_string(platform[key], f"platform {platform_name} {key}")
        portable_relative_path(platform["binary_name"])
        portable_relative_path(platform["entrypoint"])
        portable_relative_path(platform["redistributable_name"])

    runtime = value["steam_linux_runtime"]
    if not isinstance(runtime, dict):
        raise ReleaseError("steam_linux_runtime must be an object")
    _require_exact_keys(
        runtime,
        {"name", "app_id", "sdk_image", "sdk_tag", "sdk_digest"},
        "Steam Linux Runtime policy",
    )
    for key in ("name", "sdk_image", "sdk_tag", "sdk_digest"):
        _require_string(runtime[key], f"Steam Linux Runtime {key}")
    if not _is_plain_int(runtime["app_id"]) or runtime["app_id"] <= 0:
        raise ReleaseError("Steam Linux Runtime app_id must be a positive integer")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", runtime["sdk_digest"]):
        raise ReleaseError("Steam Linux Runtime sdk_digest must be a SHA-256 digest")
    return value


def portable_relative_path(raw: str) -> str:
    if not isinstance(raw, str) or not raw:
        raise ReleaseError("portable path must be a non-empty string")
    if raw != unicodedata.normalize("NFC", raw):
        raise ReleaseError(f"path is not NFC-normalized: {raw!r}")
    if "\\" in raw or "\x00" in raw or not raw.isascii():
        raise ReleaseError(f"path is not portable ASCII with forward slashes: {raw!r}")
    if not PORTABLE_PATH_RE.fullmatch(raw):
        raise ReleaseError(f"path contains unsupported characters: {raw!r}")
    path = PurePosixPath(raw)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise ReleaseError(f"path is not a safe relative path: {raw!r}")
    if any(part.endswith((" ", ".")) for part in path.parts):
        raise ReleaseError(f"path has a Windows-ambiguous component: {raw!r}")
    if any(
        part.split(".", 1)[0].casefold() in WINDOWS_RESERVED_NAMES
        for part in path.parts
    ):
        raise ReleaseError(f"path has a Windows-reserved component: {raw!r}")
    if any(":" in part for part in path.parts):
        raise ReleaseError(f"path has a Windows-reserved separator: {raw!r}")
    if len(raw.encode("ascii")) > MAX_PORTABLE_PATH_BYTES:
        raise ReleaseError(f"path exceeds {MAX_PORTABLE_PATH_BYTES} bytes: {raw!r}")
    return path.as_posix()


def ensure_casefold_unique(paths: Iterable[str], context: str) -> list[str]:
    normalized = sorted(portable_relative_path(path) for path in paths)
    seen: dict[str, str] = {}
    for path in normalized:
        key = path.casefold()
        previous = seen.get(key)
        if previous is not None and previous != path:
            raise ReleaseError(
                f"{context} has case-insensitive collision {previous!r} / {path!r}"
            )
        if previous is not None:
            raise ReleaseError(f"{context} contains duplicate {path!r}")
        seen[key] = path
    return normalized


def validate_release_label(label: str) -> str:
    if not isinstance(label, str) or not RELEASE_LABEL_RE.fullmatch(label):
        raise ReleaseError(
            "release label must match [A-Za-z0-9][A-Za-z0-9._+-]{0,63}"
        )
    if len(label.encode("ascii")) > MAX_RELEASE_LABEL_BYTES:
        raise ReleaseError("release label exceeds 64 ASCII bytes")
    return label


def validate_u32_id(value: int | str, context: str) -> int:
    if isinstance(value, str):
        if not value or len(value) > 10 or not value.isascii() or not value.isdigit():
            raise ReleaseError(f"{context} must be a non-zero decimal u32")
        parsed = int(value)
    elif _is_plain_int(value):
        parsed = value
    else:
        raise ReleaseError(f"{context} must be a non-zero decimal u32")
    if parsed <= 0 or parsed > 0xFFFF_FFFF:
        raise ReleaseError(f"{context} must be a non-zero decimal u32")
    return parsed


def validate_release_inputs(
    release_label: str,
    app_id: int | str,
    depot_ids: Mapping[str, int | str] | None = None,
    *,
    macos_bundle_id: str | None = None,
    macos_bundle_version: str | None = None,
    macos_min_version: str | None = None,
) -> dict[str, Any]:
    label = validate_release_label(release_label)
    parsed_app_id = validate_u32_id(app_id, "Steam App ID")
    if parsed_app_id == SPACEWAR_APP_ID:
        raise ReleaseError("Spacewar App ID 480 is forbidden for release candidates")
    parsed_depots: dict[str, int] = {}
    if depot_ids is not None:
        expected = {"windows-x86_64", "linux-x86_64", "macos-universal"}
        _require_exact_keys(dict(depot_ids), expected, "Steam depot IDs")
        for platform, depot_id in depot_ids.items():
            parsed_depots[platform] = validate_u32_id(
                depot_id, f"{platform} Steam depot ID"
            )
        all_ids = [parsed_app_id, *parsed_depots.values()]
        if len(all_ids) != len(set(all_ids)):
            raise ReleaseError("Steam App ID and depot IDs must all be distinct")

    mac_values = (macos_bundle_id, macos_bundle_version, macos_min_version)
    if any(value is not None for value in mac_values):
        if not all(value is not None for value in mac_values):
            raise ReleaseError(
                "macOS bundle ID, bundle version, and minimum version are all required"
            )
        assert macos_bundle_id is not None
        assert macos_bundle_version is not None
        assert macos_min_version is not None
        if not BUNDLE_ID_RE.fullmatch(macos_bundle_id):
            raise ReleaseError("macOS bundle ID must be a three-part reverse-DNS name")
        if not APPLE_VERSION_RE.fullmatch(macos_bundle_version):
            raise ReleaseError("macOS bundle version must contain one to three integers")
        if not APPLE_VERSION_RE.fullmatch(macos_min_version):
            raise ReleaseError(
                "macOS minimum version must contain one to three integers"
            )
    return {
        "release_label": label,
        "app_id": parsed_app_id,
        "depot_ids": parsed_depots,
        "macos_bundle_id": macos_bundle_id,
        "macos_bundle_version": macos_bundle_version,
        "macos_min_version": macos_min_version,
    }


def _run(
    arguments: Sequence[str],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
    timeout: int = 30,
) -> subprocess.CompletedProcess[bytes]:
    try:
        return subprocess.run(
            list(arguments),
            cwd=cwd,
            env=dict(env) if env is not None else None,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ReleaseError(f"command failed to execute {arguments[0]!r}: {error}") from error


def git_output(root: Path, arguments: Sequence[str]) -> bytes:
    result = _run(["git", *arguments], cwd=root)
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise ReleaseError(f"git {' '.join(arguments)} failed: {detail}")
    return result.stdout


def ensure_clean_git_tree(root: Path) -> str:
    root = root.resolve()
    top = Path(
        git_output(root, ["rev-parse", "--show-toplevel"])
        .decode("utf-8", "strict")
        .strip()
    ).resolve()
    if top != root:
        raise ReleaseError(f"source root {root} is not Git top-level {top}")
    status_output = git_output(
        root,
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )
    if status_output:
        preview = status_output.decode("utf-8", "replace").splitlines()[:12]
        raise ReleaseError(
            "release staging requires a clean Git tree; found:\n" + "\n".join(preview)
        )
    submodules = git_output(root, ["submodule", "status", "--recursive"])
    for line in submodules.decode("utf-8", "replace").splitlines():
        if line and line[0] in "-+U":
            raise ReleaseError(f"submodule is not at its recorded clean commit: {line}")
    commit = (
        git_output(root, ["rev-parse", "--verify", "HEAD"])
        .decode("ascii", "strict")
        .strip()
        .lower()
    )
    if not COMMIT_RE.fullmatch(commit):
        raise ReleaseError(f"Git returned invalid commit {commit!r}")
    return commit


def tracked_paths(root: Path) -> list[str]:
    raw = git_output(root, ["ls-files", "-z"])
    try:
        values = raw.decode("utf-8", "strict").split("\0")
    except UnicodeDecodeError as error:
        raise ReleaseError("tracked paths must be valid UTF-8") from error
    return ensure_casefold_unique((value for value in values if value), "tracked files")


def _is_git_lfs_pointer(path: Path) -> bool:
    try:
        with path.open("rb") as source:
            return source.read(128).startswith(
                b"version https://git-lfs.github.com/spec/v1"
            )
    except OSError as error:
        raise ReleaseError(f"could not inspect tracked file {path}: {error}") from error


def audit_source(
    root: Path, policy: Mapping[str, Any], *, require_clean: bool = True
) -> dict[str, Any]:
    root = root.resolve()
    commit = ensure_clean_git_tree(root) if require_clean else (
        git_output(root, ["rev-parse", "--verify", "HEAD"])
        .decode("ascii", "strict")
        .strip()
        .lower()
    )
    if not COMMIT_RE.fullmatch(commit):
        raise ReleaseError(f"invalid source commit {commit!r}")
    tracked = tracked_paths(root)
    tracked_set = set(tracked)
    required = set(policy["required_source_paths"])
    required.update(policy["embedded_build_only_paths"])
    required.update(policy["required_runtime_assets"])
    required.update(TOOL_SOURCE_PATHS)
    missing = sorted(required - tracked_set)
    if missing:
        raise ReleaseError(f"required release sources are not tracked: {missing}")

    embedded = set(policy["embedded_build_only_paths"])
    asset_paths = sorted(
        path
        for path in tracked
        if path.startswith("assets/") and path not in embedded
    )
    required_assets = set(policy["required_runtime_assets"])
    if not required_assets.issubset(asset_paths):
        raise ReleaseError("required runtime assets were excluded from the payload")
    if not asset_paths:
        raise ReleaseError("release payload contains no tracked runtime assets")

    for relative in sorted(required | set(asset_paths)):
        absolute = root / relative
        if absolute.is_symlink() or not absolute.is_file():
            raise ReleaseError(f"release source must be a regular file: {relative}")
        if absolute.stat().st_size <= 0:
            raise ReleaseError(f"release source must be non-empty: {relative}")
        if _is_git_lfs_pointer(absolute):
            raise ReleaseError(f"Git LFS pointer was not materialized: {relative}")
    return {
        "source_commit": commit,
        "tracked_file_count": len(tracked),
        "runtime_assets": asset_paths,
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
    except OSError as error:
        raise ReleaseError(f"could not hash {path}: {error}") from error
    return digest.hexdigest()


def _read_header(path: Path, limit: int = 4096) -> bytes:
    try:
        with path.open("rb") as source:
            return source.read(limit)
    except OSError as error:
        raise ReleaseError(f"could not read binary header {path}: {error}") from error


def validate_pe_x86_64(path: Path) -> None:
    header = _read_header(path)
    if len(header) < 64 or header[:2] != b"MZ":
        raise ReleaseError(f"{path} is not a PE executable")
    pe_offset = struct.unpack_from("<I", header, 0x3C)[0]
    if pe_offset + 6 > len(header) or header[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ReleaseError(f"{path} has an invalid PE header")
    machine = struct.unpack_from("<H", header, pe_offset + 4)[0]
    if machine != 0x8664:
        raise ReleaseError(f"{path} is not PE x86_64")


def validate_elf_x86_64(path: Path) -> None:
    header = _read_header(path, 128)
    if len(header) < 20 or header[:4] != b"\x7fELF":
        raise ReleaseError(f"{path} is not an ELF executable")
    if header[4] != 2 or header[5] != 1:
        raise ReleaseError(f"{path} is not little-endian ELF64")
    machine = struct.unpack_from("<H", header, 18)[0]
    if machine != 62:
        raise ReleaseError(f"{path} is not ELF x86_64")


def macho_fat_architectures(path: Path) -> set[int]:
    header = _read_header(path, 4096)
    if len(header) < 8:
        raise ReleaseError(f"{path} is not a Mach-O universal binary")
    magic = header[:4]
    formats = {
        b"\xca\xfe\xba\xbe": (">", 20),
        b"\xbe\xba\xfe\xca": ("<", 20),
        b"\xca\xfe\xba\xbf": (">", 32),
        b"\xbf\xba\xfe\xca": ("<", 32),
    }
    if magic not in formats:
        raise ReleaseError(f"{path} is not a Mach-O universal binary")
    endian, record_size = formats[magic]
    count = struct.unpack_from(f"{endian}I", header, 4)[0]
    if count <= 0 or count > 32 or 8 + count * record_size > len(header):
        raise ReleaseError(f"{path} has an invalid Mach-O fat header")
    return {
        struct.unpack_from(f"{endian}I", header, 8 + index * record_size)[0]
        for index in range(count)
    }


def validate_binary_format(path: Path, format_name: str) -> None:
    if path.is_symlink() or not path.is_file() or path.stat().st_size <= 0:
        raise ReleaseError(f"binary input must be a non-empty regular file: {path}")
    if format_name == "pe-x86_64":
        validate_pe_x86_64(path)
    elif format_name == "elf-x86_64":
        validate_elf_x86_64(path)
    elif format_name == "macho-universal-x86_64-arm64":
        architectures = macho_fat_architectures(path)
        required = {0x0100_0007, 0x0100_000C}
        if not required.issubset(architectures):
            raise ReleaseError(
                f"{path} lacks universal x86_64/arm64 slices: {architectures}"
            )
    else:
        raise ReleaseError(f"unknown binary format policy {format_name!r}")


def validate_identity(
    identity: Any,
    policy: Mapping[str, Any],
    *,
    expected_release_label: str | None = None,
    expected_app_id: int | None = None,
) -> dict[str, Any]:
    if not isinstance(identity, dict):
        raise ReleaseError("release identity must be a JSON object")
    _require_exact_keys(identity, IDENTITY_KEYS, "release identity")
    if identity["schema_version"] != RELEASE_IDENTITY_SCHEMA_VERSION:
        raise ReleaseError(
            f"unsupported release identity schema {identity['schema_version']!r}"
        )
    if identity["product_name"] != policy["product_name"]:
        raise ReleaseError("release identity product_name does not match policy")
    if identity["package_name"] != policy["package_name"]:
        raise ReleaseError("release identity package_name does not match policy")
    product_version = _require_string(
        identity["product_version"], "release identity product_version"
    )
    if len(product_version) > 64 or any(ord(char) < 0x20 for char in product_version):
        raise ReleaseError("release identity product_version is invalid")
    label = validate_release_label(identity["release_label"])
    if expected_release_label is not None and label != validate_release_label(
        expected_release_label
    ):
        raise ReleaseError("release identity release_label does not match candidate")
    if identity["shipping"] is not True:
        raise ReleaseError("release identity is not a shipping build")
    app_id = validate_u32_id(identity["steam_app_id"], "compiled Steam App ID")
    if app_id == SPACEWAR_APP_ID:
        raise ReleaseError("shipping identity contains Spacewar App ID 480")
    if expected_app_id is not None and app_id != validate_u32_id(
        expected_app_id, "expected Steam App ID"
    ):
        raise ReleaseError("release identity Steam App ID does not match candidate")
    if identity["steam_depot_build_id"] is not None:
        raise ReleaseError("pre-upload release identity must have null depot build ID")
    for key in (
        "protocol_version",
        "simulation_version",
        "rng_scheme_version",
        "replay_format_version",
        "snapshot_schema_version",
    ):
        value = identity[key]
        if not _is_plain_int(value) or value <= 0 or value > 0xFFFF:
            raise ReleaseError(f"release identity {key} must be a non-zero u16")
    if not isinstance(identity["compatibility_build_id"], str) or not HEX_32_RE.fullmatch(
        identity["compatibility_build_id"]
    ):
        raise ReleaseError("release identity compatibility_build_id is invalid")
    if not isinstance(identity["gameplay_content_hash"], str) or not HEX_64_RE.fullmatch(
        identity["gameplay_content_hash"]
    ):
        raise ReleaseError("release identity gameplay_content_hash is invalid")
    return dict(identity)


def sanitized_identity_environment(platform: str, stage_root: Path) -> dict[str, str]:
    blocked = {
        "AFC_STEAM_APP_ID",
        "AFC_STEAM_DEV_SPACEWAR_480",
        "STEAMAPPID",
        "STEAMGAMEID",
        "STEAM_APP_ID",
        "BEVY_ASSET_ROOT",
        "CARGO_MANIFEST_DIR",
    }
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.upper().startswith(("AFC_", "DYLD_", "LD_"))
        and key.upper() not in blocked
    }
    if platform == "linux-x86_64":
        environment["LD_LIBRARY_PATH"] = str(stage_root)
    return environment


def read_staged_identity(
    executable: Path, *, platform: str, stage_root: Path
) -> dict[str, Any]:
    environment = sanitized_identity_environment(platform, stage_root)
    if platform == "macos-universal":
        environment["DYLD_LIBRARY_PATH"] = str(executable.parent)
    with tempfile.TemporaryDirectory(prefix="afc-release-identity-cwd-") as temporary:
        result = _run(
            [str(executable), "--release-identity"],
            cwd=Path(temporary),
            env=environment,
            timeout=15,
        )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise ReleaseError(
            f"staged executable --release-identity failed ({result.returncode}): {detail}"
        )
    if result.stderr.strip():
        raise ReleaseError("staged executable wrote unexpected stderr for --release-identity")
    if not result.stdout or len(result.stdout) > MAX_IDENTITY_OUTPUT_BYTES:
        raise ReleaseError("staged executable release identity output has invalid size")
    try:
        text = result.stdout.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ReleaseError("staged executable release identity is not UTF-8") from error
    lines = text.splitlines()
    if len(lines) != 1:
        raise ReleaseError("staged executable must print exactly one identity JSON line")
    try:
        value = json.loads(lines[0])
    except json.JSONDecodeError as error:
        raise ReleaseError(f"staged executable emitted invalid identity JSON: {error}") from error
    if not isinstance(value, dict):
        raise ReleaseError("staged executable identity JSON must be an object")
    return value


def platform_layout(
    policy: Mapping[str, Any], platform: str
) -> dict[str, Any]:
    if platform not in policy["platforms"]:
        raise ReleaseError(f"unsupported release platform {platform!r}")
    platform_policy = policy["platforms"][platform]
    binary_relative = platform_policy["binary_name"]
    if platform == "macos-universal":
        binary_parent = PurePosixPath(binary_relative).parent
        redistributable_relative = (
            binary_parent / platform_policy["redistributable_name"]
        ).as_posix()
        asset_root = (binary_parent / "assets").as_posix()
        info_plist = "Animal Fighter Club.app/Contents/Info.plist"
        executable_paths = {binary_relative, redistributable_relative}
    else:
        redistributable_relative = platform_policy["redistributable_name"]
        asset_root = "assets"
        info_plist = None
        executable_paths = {binary_relative}
        if platform == "linux-x86_64":
            executable_paths.update(
                {platform_policy["entrypoint"], redistributable_relative}
            )
    return {
        "policy": platform_policy,
        "binary": binary_relative,
        "redistributable": redistributable_relative,
        "asset_root": asset_root,
        "entrypoint": platform_policy["entrypoint"],
        "identity_executable": binary_relative,
        "info_plist": info_plist,
        "executable_paths": executable_paths,
    }


def staged_asset_path(layout: Mapping[str, Any], source_asset_path: str) -> str:
    source = PurePosixPath(portable_relative_path(source_asset_path))
    if not source.parts or source.parts[0] != "assets":
        raise ReleaseError(f"runtime asset is outside assets/: {source_asset_path}")
    return (PurePosixPath(layout["asset_root"]) / PurePosixPath(*source.parts[1:])).as_posix()


def _copy_regular_file(source: Path, destination: Path, *, executable: bool) -> None:
    if source.is_symlink() or not source.is_file() or source.stat().st_size <= 0:
        raise ReleaseError(f"input must be a non-empty regular file: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    os.chmod(destination, 0o755 if executable else 0o644)


def _macos_info_plist(
    identity: Mapping[str, Any],
    *,
    bundle_id: str,
    bundle_version: str,
    minimum_version: str,
) -> bytes:
    values = validate_release_inputs(
        identity["release_label"],
        identity["steam_app_id"],
        macos_bundle_id=bundle_id,
        macos_bundle_version=bundle_version,
        macos_min_version=minimum_version,
    )
    assert values["macos_bundle_id"] is not None
    product_version = _require_string(
        identity["product_version"], "release identity product_version"
    )
    if not APPLE_VERSION_RE.fullmatch(product_version):
        raise ReleaseError(
            "macOS CFBundleShortVersionString must contain one to three integers"
        )
    data = {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": "Animal Fighter Club",
        "CFBundleExecutable": "ffc-prototype",
        "CFBundleIdentifier": bundle_id,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "Animal Fighter Club",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": product_version,
        "CFBundleVersion": bundle_version,
        "LSApplicationCategoryType": "public.app-category.action-games",
        "LSMinimumSystemVersion": minimum_version,
        "NSHighResolutionCapable": True,
    }
    return plistlib.dumps(data, fmt=plistlib.FMT_XML, sort_keys=True)


def _all_stage_files(stage_root: Path) -> list[tuple[str, Path]]:
    files: list[tuple[str, Path]] = []
    for path in stage_root.rglob("*"):
        if path.is_symlink():
            raise ReleaseError(f"stage contains a symlink: {path}")
        if path.is_file():
            relative = path.relative_to(stage_root).as_posix()
            files.append((portable_relative_path(relative), path))
    files.sort(key=lambda item: item[0])
    ensure_casefold_unique((relative for relative, _ in files), "stage files")
    return files


def _forbidden_payload_path(
    relative: str, policy: Mapping[str, Any], layout: Mapping[str, Any]
) -> str | None:
    path = PurePosixPath(relative)
    if any(part.casefold() == "web_dist" for part in path.parts):
        return "web_dist is not a native-depot payload"
    forbidden_names = {name.casefold() for name in policy["forbidden_basenames"]}
    if path.name.casefold() in forbidden_names:
        return f"forbidden basename {path.name}"
    lowered = relative.casefold()
    for suffix in policy["forbidden_suffixes"]:
        folded_suffix = suffix.casefold()
        if lowered.endswith(folded_suffix) or any(
            part.casefold().endswith(folded_suffix) for part in path.parts
        ):
            return f"forbidden suffix {suffix}"
    for embedded in policy["embedded_build_only_paths"]:
        if not embedded.startswith("assets/"):
            continue
        if relative == staged_asset_path(layout, embedded):
            return f"embedded build-only source {embedded}"
    if path.name.casefold() in {
        "afc-dedicated",
        "afc-dedicated.exe",
        "afc-multiplayer-profile",
    }:
        return "non-player binary"
    return None


def _seal_stage(
    stage_root: Path,
    *,
    policy: Mapping[str, Any],
    platform: str,
    source_commit: str,
    identity: Mapping[str, Any],
) -> dict[str, Any]:
    layout = platform_layout(policy, platform)
    identity_path = stage_root / "release-identity.json"
    atomic_write_bytes(identity_path, deterministic_json_bytes(identity))

    payload: list[dict[str, Any]] = []
    for relative, absolute in _all_stage_files(stage_root):
        if relative in {"release-manifest.json", "SHA256SUMS"}:
            raise ReleaseError(f"stage was already sealed: {relative}")
        forbidden = _forbidden_payload_path(relative, policy, layout)
        if forbidden is not None:
            raise ReleaseError(f"forbidden stage path {relative}: {forbidden}")
        payload.append(
            {
                "path": relative,
                "bytes": absolute.stat().st_size,
                "sha256": sha256_file(absolute),
                "executable": relative in layout["executable_paths"],
            }
        )
    payload.sort(key=lambda record: record["path"])

    redist_relative = layout["redistributable"]
    redist_record = next(
        (record for record in payload if record["path"] == redist_relative), None
    )
    if redist_record is None:
        raise ReleaseError("stage is missing its Steam API redistributable")
    steam_runtime = (
        dict(policy["steam_linux_runtime"])
        if platform == "linux-x86_64"
        else None
    )
    manifest = {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "platform": platform,
        "source_commit": source_commit,
        "entrypoint": layout["entrypoint"],
        "release_identity": dict(identity),
        "steam": {
            "api_redistributable": redist_relative,
            "api_redistributable_sha256": redist_record["sha256"],
            "linux_runtime": steam_runtime,
        },
        "payload": payload,
    }
    manifest_path = stage_root / "release-manifest.json"
    atomic_write_bytes(manifest_path, deterministic_json_bytes(manifest))

    checksum_entries = {
        record["path"]: record["sha256"] for record in payload
    }
    checksum_entries["release-manifest.json"] = sha256_file(manifest_path)
    checksum_text = "".join(
        f"{checksum_entries[path]} *{path}\n" for path in sorted(checksum_entries)
    )
    atomic_write_bytes(stage_root / "SHA256SUMS", checksum_text.encode("ascii"))
    return manifest


def _prepare_output_directory(output: Path) -> Path:
    output = output.resolve()
    if output.exists():
        raise ReleaseError(f"output already exists; refusing to replace it: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=f".{output.name}.tmp-", dir=output.parent))


def _publish_output_directory(temporary: Path, output: Path) -> None:
    if output.exists():
        raise ReleaseError(f"output appeared during staging: {output}")
    os.replace(temporary, output)


def stage_candidate(
    *,
    source_root: Path,
    policy: Mapping[str, Any],
    platform: str,
    binary: Path,
    redistributable: Path,
    output: Path,
    release_label: str,
    app_id: int,
    source_commit: str,
    runtime_assets: Sequence[str],
    macos_bundle_id: str | None = None,
    macos_bundle_version: str | None = None,
    macos_min_version: str | None = None,
    identity_provider: Callable[[Path, str, Path], Mapping[str, Any]] | None = None,
) -> dict[str, Any]:
    source_root = source_root.resolve()
    output = output.resolve()
    if not COMMIT_RE.fullmatch(source_commit):
        raise ReleaseError("source commit must be 40 lowercase hexadecimal characters")
    validate_release_inputs(
        release_label,
        app_id,
        macos_bundle_id=macos_bundle_id if platform == "macos-universal" else None,
        macos_bundle_version=(
            macos_bundle_version if platform == "macos-universal" else None
        ),
        macos_min_version=(
            macos_min_version if platform == "macos-universal" else None
        ),
    )
    layout = platform_layout(policy, platform)
    validate_binary_format(binary, layout["policy"]["binary_format"])
    validate_binary_format(redistributable, layout["policy"]["binary_format"])
    if redistributable.name != layout["policy"]["redistributable_name"]:
        raise ReleaseError(
            f"expected redistributable {layout['policy']['redistributable_name']}, "
            f"got {redistributable.name}"
        )
    assets = ensure_casefold_unique(runtime_assets, "runtime asset list")
    required_assets = set(policy["required_runtime_assets"])
    if not required_assets.issubset(assets):
        raise ReleaseError(
            f"runtime asset list is missing {sorted(required_assets - set(assets))}"
        )
    embedded = set(policy["embedded_build_only_paths"])
    leaked = sorted(embedded.intersection(assets))
    if leaked:
        raise ReleaseError(f"runtime asset list contains embedded files: {leaked}")

    temporary = _prepare_output_directory(output)
    try:
        binary_destination = temporary / Path(layout["binary"])
        redist_destination = temporary / Path(layout["redistributable"])
        _copy_regular_file(binary, binary_destination, executable=True)
        _copy_regular_file(
            redistributable,
            redist_destination,
            executable=platform != "windows-x86_64",
        )
        if platform == "linux-x86_64":
            atomic_write_bytes(
                temporary / layout["entrypoint"],
                LINUX_LAUNCHER.encode("ascii"),
                mode=0o755,
            )
        for relative in assets:
            source = source_root / relative
            destination = temporary / staged_asset_path(layout, relative)
            _copy_regular_file(source, destination, executable=False)

        identity_executable = temporary / layout["identity_executable"]
        if identity_provider is None:
            raw_identity = read_staged_identity(
                identity_executable, platform=platform, stage_root=temporary
            )
        else:
            raw_identity = dict(
                identity_provider(identity_executable, platform, temporary)
            )
        identity = validate_identity(
            raw_identity,
            policy,
            expected_release_label=release_label,
            expected_app_id=app_id,
        )
        if platform == "macos-universal":
            assert macos_bundle_id is not None
            assert macos_bundle_version is not None
            assert macos_min_version is not None
            assert layout["info_plist"] is not None
            atomic_write_bytes(
                temporary / layout["info_plist"],
                _macos_info_plist(
                    identity,
                    bundle_id=macos_bundle_id,
                    bundle_version=macos_bundle_version,
                    minimum_version=macos_min_version,
                ),
            )
        manifest = _seal_stage(
            temporary,
            policy=policy,
            platform=platform,
            source_commit=source_commit,
            identity=identity,
        )
        verify_stage(
            temporary,
            policy,
            expected_platform=platform,
            expected_release_label=release_label,
            expected_app_id=app_id,
            expected_source_commit=source_commit,
        )
        _publish_output_directory(temporary, output)
        return manifest
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def ensure_output_below(root: Path, output: Path, directory_name: str) -> Path:
    base = (root.resolve() / directory_name).resolve()
    output = output.resolve()
    try:
        output.relative_to(base)
    except ValueError as error:
        raise ReleaseError(f"output must be below repository-root {directory_name}/") from error
    return output


def stage_from_repository(
    *,
    root: Path,
    policy: Mapping[str, Any],
    platform: str,
    binary: Path,
    redistributable: Path,
    output: Path,
    release_label: str,
    app_id: int,
    macos_bundle_id: str | None = None,
    macos_bundle_version: str | None = None,
    macos_min_version: str | None = None,
) -> dict[str, Any]:
    output = ensure_output_below(root, output, "dist")
    audit = audit_source(root, policy, require_clean=True)
    return stage_candidate(
        source_root=root,
        policy=policy,
        platform=platform,
        binary=binary.resolve(),
        redistributable=redistributable.resolve(),
        output=output,
        release_label=release_label,
        app_id=app_id,
        source_commit=audit["source_commit"],
        runtime_assets=audit["runtime_assets"],
        macos_bundle_id=macos_bundle_id,
        macos_bundle_version=macos_bundle_version,
        macos_min_version=macos_min_version,
    )


def find_redistributable(
    search_root: Path, policy: Mapping[str, Any], platform: str
) -> Path:
    layout = platform_layout(policy, platform)
    expected = layout["policy"]["redistributable_name"]
    if not search_root.is_dir():
        raise ReleaseError(f"redistributable search root is not a directory: {search_root}")
    matches = sorted(
        path.resolve()
        for path in search_root.rglob(expected)
        if path.is_file()
        and not path.is_symlink()
        and path.parent.name == "out"
        and path.parent.parent.name.startswith("steamworks-sys-")
    )
    if len(matches) != 1:
        raise ReleaseError(
            f"expected exactly one {expected} below {search_root}, found {len(matches)}"
        )
    validate_binary_format(matches[0], layout["policy"]["binary_format"])
    return matches[0]


def _load_manifest(stage_or_manifest: Path) -> tuple[dict[str, Any], Path | None]:
    if stage_or_manifest.is_symlink():
        raise ReleaseError(f"candidate path must not be a symlink: {stage_or_manifest}")
    if stage_or_manifest.is_dir():
        path = stage_or_manifest / "release-manifest.json"
        root: Path | None = stage_or_manifest
    else:
        path = stage_or_manifest
        root = None
    value = load_json(path, context="release manifest")
    if not isinstance(value, dict):
        raise ReleaseError("release manifest root must be an object")
    return value, root


def _validate_manifest_shape(
    manifest: Mapping[str, Any], policy: Mapping[str, Any]
) -> tuple[str, dict[str, Any], list[dict[str, Any]]]:
    _require_exact_keys(manifest, MANIFEST_KEYS, "release manifest")
    if manifest["schema_version"] != MANIFEST_SCHEMA_VERSION:
        raise ReleaseError(
            f"unsupported release manifest schema {manifest['schema_version']!r}"
        )
    platform = _require_string(manifest["platform"], "release manifest platform")
    layout = platform_layout(policy, platform)
    if manifest["entrypoint"] != layout["entrypoint"]:
        raise ReleaseError("release manifest entrypoint does not match platform policy")
    commit = manifest["source_commit"]
    if not isinstance(commit, str) or not COMMIT_RE.fullmatch(commit):
        raise ReleaseError("release manifest source_commit is invalid")
    identity = validate_identity(manifest["release_identity"], policy)
    steam = manifest["steam"]
    if not isinstance(steam, dict):
        raise ReleaseError("release manifest steam must be an object")
    _require_exact_keys(
        steam,
        {
            "api_redistributable",
            "api_redistributable_sha256",
            "linux_runtime",
        },
        "release manifest steam",
    )
    if steam["api_redistributable"] != layout["redistributable"]:
        raise ReleaseError("manifest Steam redistributable path is invalid")
    if not isinstance(steam["api_redistributable_sha256"], str) or not HEX_64_RE.fullmatch(
        steam["api_redistributable_sha256"]
    ):
        raise ReleaseError("manifest Steam redistributable hash is invalid")
    expected_runtime = (
        policy["steam_linux_runtime"] if platform == "linux-x86_64" else None
    )
    if steam["linux_runtime"] != expected_runtime:
        raise ReleaseError("manifest Steam Linux Runtime policy is invalid")
    payload_value = manifest["payload"]
    if not isinstance(payload_value, list) or not payload_value:
        raise ReleaseError("release manifest payload must be a non-empty array")
    payload: list[dict[str, Any]] = []
    previous = ""
    for index, record in enumerate(payload_value):
        if not isinstance(record, dict):
            raise ReleaseError(f"payload record {index} must be an object")
        _require_exact_keys(
            record, {"path", "bytes", "sha256", "executable"}, f"payload record {index}"
        )
        relative = portable_relative_path(record["path"])
        if index and relative <= previous:
            raise ReleaseError("payload records must be unique and strictly path-sorted")
        previous = relative
        if not _is_plain_int(record["bytes"]) or record["bytes"] <= 0:
            raise ReleaseError(f"payload record {relative} has invalid byte size")
        if not isinstance(record["sha256"], str) or not HEX_64_RE.fullmatch(
            record["sha256"]
        ):
            raise ReleaseError(f"payload record {relative} has invalid SHA-256")
        if not isinstance(record["executable"], bool):
            raise ReleaseError(f"payload record {relative} executable must be boolean")
        payload.append(dict(record))
    ensure_casefold_unique(
        (record["path"] for record in payload), "manifest payload paths"
    )
    return platform, identity, payload


def parse_checksums(path: Path) -> dict[str, str]:
    try:
        contents = path.read_text(encoding="ascii")
    except (OSError, UnicodeError) as error:
        raise ReleaseError(f"could not read checksum file {path}: {error}") from error
    if not contents or not contents.endswith("\n"):
        raise ReleaseError("SHA256SUMS must be non-empty and LF-terminated")
    result: dict[str, str] = {}
    previous = ""
    for line in contents.splitlines():
        match = CHECKSUM_LINE_RE.fullmatch(line)
        if match is None:
            raise ReleaseError(f"invalid SHA256SUMS line: {line!r}")
        digest, relative = match.groups()
        relative = portable_relative_path(relative)
        if result or previous:
            if relative <= previous:
                raise ReleaseError("SHA256SUMS paths must be unique and sorted")
        if relative in result:
            raise ReleaseError(f"duplicate SHA256SUMS path {relative}")
        result[relative] = digest
        previous = relative
    return result


def verify_stage(
    stage_root: Path,
    policy: Mapping[str, Any],
    *,
    expected_platform: str | None = None,
    expected_release_label: str | None = None,
    expected_app_id: int | None = None,
    expected_source_commit: str | None = None,
) -> dict[str, Any]:
    if stage_root.is_symlink():
        raise ReleaseError(f"stage root must not be a symlink: {stage_root}")
    stage_root = stage_root.resolve()
    if not stage_root.is_dir():
        raise ReleaseError(f"stage root is not a regular directory: {stage_root}")
    manifest_value = load_json(
        stage_root / "release-manifest.json", context="release manifest"
    )
    if not isinstance(manifest_value, dict):
        raise ReleaseError("release manifest root must be an object")
    platform, identity, payload = _validate_manifest_shape(manifest_value, policy)
    layout = platform_layout(policy, platform)
    if expected_platform is not None and platform != expected_platform:
        raise ReleaseError(f"expected platform {expected_platform}, found {platform}")
    validate_identity(
        identity,
        policy,
        expected_release_label=expected_release_label,
        expected_app_id=expected_app_id,
    )
    if expected_source_commit is not None and manifest_value["source_commit"] != (
        expected_source_commit
    ):
        raise ReleaseError("stage source commit does not match expected commit")

    actual_files = {relative: absolute for relative, absolute in _all_stage_files(stage_root)}
    payload_paths = {record["path"] for record in payload}
    required_payload_paths = {
        layout["binary"],
        layout["redistributable"],
        "release-identity.json",
        *(
            staged_asset_path(layout, source)
            for source in policy["required_runtime_assets"]
        ),
    }
    if platform == "linux-x86_64":
        required_payload_paths.add(layout["entrypoint"])
    if layout["info_plist"] is not None:
        required_payload_paths.add(layout["info_plist"])
    if not required_payload_paths.issubset(payload_paths):
        raise ReleaseError(
            "manifest is missing required structural payload paths: "
            f"{sorted(required_payload_paths - payload_paths)}"
        )
    expected_files = payload_paths | {"release-manifest.json", "SHA256SUMS"}
    if set(actual_files) != expected_files:
        missing = sorted(expected_files - set(actual_files))
        extra = sorted(set(actual_files) - expected_files)
        raise ReleaseError(f"stage file set mismatch; missing={missing}, extra={extra}")

    record_by_path = {record["path"]: record for record in payload}
    for relative, record in record_by_path.items():
        expected_executable = relative in layout["executable_paths"]
        if record["executable"] is not expected_executable:
            raise ReleaseError(
                f"payload executable flag differs from platform policy: {relative}"
            )
        forbidden = _forbidden_payload_path(relative, policy, layout)
        if forbidden is not None:
            raise ReleaseError(f"forbidden stage path {relative}: {forbidden}")
        absolute = actual_files[relative]
        if absolute.stat().st_size != record["bytes"]:
            raise ReleaseError(f"payload size mismatch: {relative}")
        if sha256_file(absolute) != record["sha256"]:
            raise ReleaseError(f"payload checksum mismatch: {relative}")
        if os.name != "nt" and record["executable"]:
            if not (stat.S_IMODE(absolute.stat().st_mode) & 0o111):
                raise ReleaseError(f"payload executable bit is missing: {relative}")

    required_staged_assets = {
        staged_asset_path(layout, source) for source in policy["required_runtime_assets"]
    }
    if not required_staged_assets.issubset(payload_paths):
        raise ReleaseError(
            f"stage is missing required Steam Input assets: "
            f"{sorted(required_staged_assets - payload_paths)}"
        )
    for embedded in policy["embedded_build_only_paths"]:
        if embedded.startswith("assets/"):
            leaked = staged_asset_path(layout, embedded)
            if leaked in payload_paths:
                raise ReleaseError(f"embedded build-only asset leaked into stage: {embedded}")

    identity_from_file = load_json(
        stage_root / "release-identity.json", context="staged release identity"
    )
    if identity_from_file != identity:
        raise ReleaseError("release-identity.json differs from manifest identity")

    checksum_map = parse_checksums(stage_root / "SHA256SUMS")
    expected_checksum_paths = payload_paths | {"release-manifest.json"}
    if set(checksum_map) != expected_checksum_paths:
        raise ReleaseError("SHA256SUMS file set does not match sealed payload")
    for relative, expected_digest in checksum_map.items():
        if sha256_file(actual_files[relative]) != expected_digest:
            raise ReleaseError(f"SHA256SUMS mismatch: {relative}")
    redist_record = record_by_path[layout["redistributable"]]
    if (
        redist_record["sha256"]
        != manifest_value["steam"]["api_redistributable_sha256"]
    ):
        raise ReleaseError("Steam redistributable manifest hashes disagree")

    validate_binary_format(
        stage_root / layout["binary"], layout["policy"]["binary_format"]
    )
    validate_binary_format(
        stage_root / layout["redistributable"], layout["policy"]["binary_format"]
    )
    entrypoint = stage_root / layout["entrypoint"]
    if platform == "macos-universal":
        if not entrypoint.is_dir():
            raise ReleaseError("macOS app-bundle entrypoint is missing")
        info_path = stage_root / str(layout["info_plist"])
        try:
            info = plistlib.loads(info_path.read_bytes())
        except (OSError, plistlib.InvalidFileException) as error:
            raise ReleaseError(f"invalid macOS Info.plist: {error}") from error
        if info.get("CFBundleExecutable") != "ffc-prototype":
            raise ReleaseError("macOS Info.plist executable is invalid")
        if info.get("CFBundleShortVersionString") != identity["product_version"]:
            raise ReleaseError("macOS Info.plist product version is invalid")
    elif not entrypoint.is_file():
        raise ReleaseError("stage entrypoint is missing")

    return dict(manifest_value)


def deterministic_archive(
    stage_root: Path,
    output: Path,
    policy: Mapping[str, Any],
) -> dict[str, Any]:
    manifest = verify_stage(stage_root, policy)
    stage_root = stage_root.resolve()
    output = output.resolve()
    if output.exists() or output.with_suffix(output.suffix + ".sha256").exists():
        raise ReleaseError(f"archive output already exists: {output}")
    if output.suffix.lower() != ".zip":
        raise ReleaseError("release archives must use the .zip suffix")
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{output.name}.tmp-", dir=output.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    executable = {
        record["path"] for record in manifest["payload"] if record["executable"]
    }
    try:
        with zipfile.ZipFile(
            temporary,
            mode="w",
            compression=zipfile.ZIP_STORED,
            strict_timestamps=True,
        ) as archive:
            for relative, absolute in _all_stage_files(stage_root):
                info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_STORED
                info.create_system = 3
                mode = 0o755 if relative in executable else 0o644
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.flag_bits |= 0x800
                archive.writestr(info, absolute.read_bytes())
        os.replace(temporary, output)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise
    digest = sha256_file(output)
    sidecar = output.with_suffix(output.suffix + ".sha256")
    atomic_write_bytes(sidecar, f"{digest} *{output.name}\n".encode("ascii"))
    return {
        "archive": str(output),
        "sha256": digest,
        "bytes": output.stat().st_size,
        "platform": manifest["platform"],
    }


def compare_identities(
    stages_or_manifests: Sequence[Path], policy: Mapping[str, Any]
) -> dict[str, Any]:
    expected_platforms = set(policy["platforms"])
    if len(stages_or_manifests) != len(expected_platforms):
        raise ReleaseError(
            "compare-identities requires exactly one candidate for every "
            f"supported platform: {sorted(expected_platforms)}"
        )
    manifests: list[dict[str, Any]] = []
    for path in stages_or_manifests:
        manifest, root = _load_manifest(path)
        _validate_manifest_shape(manifest, policy)
        if root is not None:
            manifest = verify_stage(root, policy)
        manifests.append(manifest)
    reference = manifests[0]
    seen_platforms: set[str] = set()
    for manifest in manifests:
        platform = manifest["platform"]
        if platform in seen_platforms:
            raise ReleaseError(f"duplicate candidate platform {platform}")
        seen_platforms.add(platform)
        if manifest["source_commit"] != reference["source_commit"]:
            raise ReleaseError("candidate source commits differ")
        if manifest["release_identity"] != reference["release_identity"]:
            raise ReleaseError("candidate release identities differ")
    if seen_platforms != expected_platforms:
        raise ReleaseError(
            "candidate platform set differs from release policy; "
            f"expected={sorted(expected_platforms)}, found={sorted(seen_platforms)}"
        )
    return {
        "platforms": sorted(seen_platforms),
        "source_commit": reference["source_commit"],
        "release_identity": reference["release_identity"],
    }


def vdf_escape(value: str) -> str:
    if not isinstance(value, str) or not value:
        raise ReleaseError("VDF value must be a non-empty string")
    if any(ord(char) < 0x20 for char in value):
        raise ReleaseError("VDF values cannot contain control characters")
    return value.replace("\\", "\\\\").replace('"', '\\"')


def render_template(
    template_path: Path, replacements: Mapping[str, str]
) -> str:
    try:
        template = template_path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ReleaseError(f"could not read VDF template {template_path}: {error}") from error
    placeholders = PLACEHOLDER_RE.findall(template)
    if set(placeholders) != set(replacements):
        raise ReleaseError(
            f"VDF template placeholders differ; template={sorted(set(placeholders))}, "
            f"provided={sorted(replacements)}"
        )
    if any(placeholders.count(name) != 1 for name in set(placeholders)):
        raise ReleaseError("each VDF template placeholder must occur exactly once")
    rendered = template
    for name, value in replacements.items():
        rendered = rendered.replace(f"@@{name}@@", vdf_escape(value))
    if PLACEHOLDER_RE.search(rendered):
        raise ReleaseError("rendered VDF contains unresolved placeholders")
    validate_preview_vdf(rendered)
    return rendered


def validate_preview_vdf(contents: str) -> None:
    if not contents or not contents.endswith("\n"):
        raise ReleaseError("VDF must be non-empty and LF-terminated")
    lowered = contents.casefold()
    for banned in (
        "+login",
        "run_app_build",
        "setlive",
        "steamcmd",
    ):
        if banned in lowered:
            raise ReleaseError(f"VDF contains forbidden upload/credential token {banned!r}")
    if re.search(r'(?i)"(?:username|password)"\s+"', contents):
        raise ReleaseError("VDF contains a forbidden credential field")
    if re.search(r'(?i)"AppBuild"', contents):
        preview_values = re.findall(r'(?i)"Preview"\s*"([^"\r\n]*)"', contents)
        if preview_values != ["1"]:
            raise ReleaseError(
                "AppBuild VDF must contain exactly one Preview field set to 1"
            )
    depth = 0
    in_string = False
    escaped = False
    for char in contents:
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth < 0:
                raise ReleaseError("VDF has an unmatched closing brace")
        elif ord(char) < 0x20 and char not in "\n\r\t":
            raise ReleaseError("VDF has an invalid control character")
    if in_string or escaped or depth != 0:
        raise ReleaseError("VDF has unbalanced strings or braces")


def _paths_overlap(left: Path, right: Path) -> bool:
    try:
        left.relative_to(right)
        return True
    except ValueError:
        pass
    try:
        right.relative_to(left)
        return True
    except ValueError:
        return False


def render_steam_vdfs(
    *,
    root: Path,
    policy: Mapping[str, Any],
    stages: Mapping[str, Path],
    app_id: int,
    depot_ids: Mapping[str, int],
    release_label: str,
    source_commit: str,
    build_output: Path,
    output: Path,
    enforce_output_location: bool = True,
) -> dict[str, Any]:
    parsed = validate_release_inputs(release_label, app_id, depot_ids)
    if not COMMIT_RE.fullmatch(source_commit):
        raise ReleaseError("Steam VDF source commit must be a full lowercase Git commit")
    expected_platforms = set(policy["platforms"])
    _require_exact_keys(dict(stages), expected_platforms, "Steam VDF stage roots")
    manifests: dict[str, dict[str, Any]] = {}
    resolved_stages: dict[str, Path] = {}
    for platform, stage in stages.items():
        manifests[platform] = verify_stage(
            stage,
            policy,
            expected_platform=platform,
            expected_release_label=release_label,
            expected_app_id=app_id,
            expected_source_commit=source_commit,
        )
        resolved_stages[platform] = stage.resolve()
    comparison = compare_identities(list(resolved_stages.values()), policy)
    if set(comparison["platforms"]) != expected_platforms:
        raise ReleaseError("Steam VDF rendering requires all three candidate platforms")

    build_output = build_output.resolve()
    output = output.resolve()
    if enforce_output_location:
        ensure_output_below(root, output, ".steam")
        ensure_output_below(root, build_output, ".steam")
    for stage in resolved_stages.values():
        if _paths_overlap(stage, build_output) or _paths_overlap(stage, output):
            raise ReleaseError("VDF/build output directories must not overlap depot content")
    if _paths_overlap(output, build_output):
        raise ReleaseError("VDF output and Steam build output must not overlap")
    if output.exists():
        raise ReleaseError(f"VDF output already exists: {output}")

    app_template = root / "packaging" / "steam" / "app_build.vdf.in"
    depot_template = root / "packaging" / "steam" / "depot_build.vdf.in"
    temporary = _prepare_output_directory(output)
    try:
        depot_files: dict[str, str] = {}
        for platform in sorted(expected_platforms):
            depot_id = parsed["depot_ids"][platform]
            file_name = f"depot_build_{depot_id}.vdf"
            depot_files[platform] = file_name
            rendered = render_template(
                depot_template,
                {
                    "DEPOT_ID": str(depot_id),
                    "CONTENT_ROOT": str(resolved_stages[platform]),
                },
            )
            atomic_write_bytes(temporary / file_name, rendered.encode("utf-8"))
        app_file_name = f"app_build_{parsed['app_id']}.vdf"
        rendered_app = render_template(
            app_template,
            {
                "APP_ID": str(parsed["app_id"]),
                "DESCRIPTION": (
                    f"AFC {parsed['release_label']} {source_commit[:12]} preview"
                ),
                "BUILD_OUTPUT": str(build_output),
                "WINDOWS_DEPOT_ID": str(
                    parsed["depot_ids"]["windows-x86_64"]
                ),
                "WINDOWS_DEPOT_VDF": depot_files["windows-x86_64"],
                "LINUX_DEPOT_ID": str(parsed["depot_ids"]["linux-x86_64"]),
                "LINUX_DEPOT_VDF": depot_files["linux-x86_64"],
                "MACOS_DEPOT_ID": str(parsed["depot_ids"]["macos-universal"]),
                "MACOS_DEPOT_VDF": depot_files["macos-universal"],
            },
        )
        atomic_write_bytes(temporary / app_file_name, rendered_app.encode("utf-8"))
        rendered_files = _all_stage_files(temporary)
        for _, path in rendered_files:
            validate_preview_vdf(path.read_text(encoding="utf-8"))
        _publish_output_directory(temporary, output)
    except Exception:
        shutil.rmtree(temporary, ignore_errors=True)
        raise
    return {
        "app_build": str(output / app_file_name),
        "depots": {
            platform: str(output / file_name)
            for platform, file_name in sorted(depot_files.items())
        },
        "preview_only": True,
        "source_commit": source_commit,
    }
