#!/usr/bin/env python3
"""Validate privacy-safe App ID 480 physical acceptance JSONL evidence.

The validator is intentionally strict. Records contain only build provenance,
fixed scenario coordinates, bounded elapsed time, and numeric cleanup counts.
They have no fields for accounts, machines, Steam identities, addresses, persona
names, tickets, payloads, or native diagnostic text.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import sys
import tempfile
import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence


SCHEMA_VERSION = 1
STEAM_APP_ID = 480
EXPECTED_COLD_STARTS = 12
EXPECTED_CYCLES = 40
MAX_COUNTDOWN_MS = 30_000
MAX_RECORD_BYTES = 8 * 1024
MAX_EVIDENCE_BYTES = 256 * 1024

PLATFORMS = ("macos", "windows")
READY_ORDERS = ("host_first", "guest_first")
CLEANUP_FIELDS = frozenset(
    {
        "tickets",
        "auth_sessions",
        "connections",
        "endpoints",
        "transports",
        "workers",
    }
)
COMMON_FIELDS = frozenset(
    {
        "schema_version",
        "steam_app_id",
        "kind",
        "host_platform",
        "guest_platform",
        "host_source_tag",
        "guest_source_tag",
        "host_source_commit",
        "guest_source_commit",
        "host_compatibility_build_id",
        "guest_compatibility_build_id",
        "host_gameplay_content_hash",
        "guest_gameplay_content_hash",
        "host_archive_sha256",
        "guest_archive_sha256",
        "host_cleanup",
        "guest_cleanup",
        "result",
    }
)
COLD_START_FIELDS = COMMON_FIELDS | {"ready_order", "pass_index", "countdown_ms"}
CYCLE_FIELDS = COMMON_FIELDS | {"cycle_index"}

TAG_PATTERN = re.compile(r"spacewar-test-[1-9][0-9]*\Z")
COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}\Z")
BUILD_ID_PATTERN = re.compile(r"[0-9a-f]{32}\Z")
DIGEST_PATTERN = re.compile(r"[0-9a-f]{64}\Z")


class AcceptanceValidationError(ValueError):
    """The evidence is malformed, incomplete, inconsistent, or unsuccessful."""


@dataclass(frozen=True)
class ExpectedBuild:
    source_tag: str
    source_commit: str
    compatibility_build_id: str
    gameplay_content_hash: str
    macos_archive_sha256: str
    windows_archive_sha256: str

    def validate(self) -> None:
        _require_pattern("expected source tag", self.source_tag, TAG_PATTERN)
        _require_pattern("expected source commit", self.source_commit, COMMIT_PATTERN)
        _require_pattern(
            "expected compatibility build ID",
            self.compatibility_build_id,
            BUILD_ID_PATTERN,
        )
        _require_pattern(
            "expected gameplay-content hash",
            self.gameplay_content_hash,
            DIGEST_PATTERN,
        )
        _require_pattern(
            "expected macOS archive SHA-256",
            self.macos_archive_sha256,
            DIGEST_PATTERN,
        )
        _require_pattern(
            "expected Windows archive SHA-256",
            self.windows_archive_sha256,
            DIGEST_PATTERN,
        )

    def archive_sha256(self, platform: str) -> str:
        if platform == "macos":
            return self.macos_archive_sha256
        if platform == "windows":
            return self.windows_archive_sha256
        raise AcceptanceValidationError(f"unsupported platform: {platform!r}")


def _require_pattern(label: str, value: object, pattern: re.Pattern[str]) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise AcceptanceValidationError(f"{label} has an invalid bounded format")
    return value


def _require_exact_keys(label: str, value: dict[str, Any], expected: frozenset[str]) -> None:
    actual = frozenset(value)
    if actual != expected:
        missing = sorted(expected - actual)
        unexpected = sorted(actual - expected)
        raise AcceptanceValidationError(
            f"{label} fields differ: missing={missing}, unexpected={unexpected}"
        )


def _require_int(label: str, value: object, minimum: int, maximum: int) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise AcceptanceValidationError(
            f"{label} must be an integer in [{minimum}, {maximum}]"
        )
    return value


def _require_cleanup(label: str, value: object) -> None:
    if not isinstance(value, dict):
        raise AcceptanceValidationError(f"{label} must be an object")
    _require_exact_keys(label, value, CLEANUP_FIELDS)
    for field in sorted(CLEANUP_FIELDS):
        _require_int(f"{label}.{field}", value[field], 0, 0)


def _require_equal(label: str, actual: object, expected: object) -> None:
    if actual != expected:
        raise AcceptanceValidationError(f"{label} does not match the sealed candidate")


def _validate_common(record: dict[str, Any], line_number: int, expected: ExpectedBuild) -> None:
    prefix = f"line {line_number}"
    _require_equal(f"{prefix} schema_version", record["schema_version"], SCHEMA_VERSION)
    _require_equal(f"{prefix} steam_app_id", record["steam_app_id"], STEAM_APP_ID)

    host = record["host_platform"]
    guest = record["guest_platform"]
    if host not in PLATFORMS or guest not in PLATFORMS or host == guest:
        raise AcceptanceValidationError(
            f"{prefix} must pair one macOS peer with one Windows peer"
        )

    for role in ("host", "guest"):
        _require_equal(
            f"{prefix} {role} source tag",
            record[f"{role}_source_tag"],
            expected.source_tag,
        )
        _require_equal(
            f"{prefix} {role} source commit",
            record[f"{role}_source_commit"],
            expected.source_commit,
        )
        _require_equal(
            f"{prefix} {role} compatibility build ID",
            record[f"{role}_compatibility_build_id"],
            expected.compatibility_build_id,
        )
        _require_equal(
            f"{prefix} {role} gameplay-content hash",
            record[f"{role}_gameplay_content_hash"],
            expected.gameplay_content_hash,
        )
        platform = record[f"{role}_platform"]
        _require_equal(
            f"{prefix} {role} archive SHA-256",
            record[f"{role}_archive_sha256"],
            expected.archive_sha256(platform),
        )
        _require_cleanup(f"{prefix} {role}_cleanup", record[f"{role}_cleanup"])


def validate_records(records: Sequence[dict[str, Any]], expected: ExpectedBuild) -> None:
    """Validate the complete 12-cold-start and 40-cycle physical campaign."""

    expected.validate()
    if len(records) != EXPECTED_COLD_STARTS + EXPECTED_CYCLES:
        raise AcceptanceValidationError(
            "evidence must contain exactly 12 cold-start and 40 cycle records"
        )

    cold_coordinates: set[tuple[str, str, int]] = set()
    cycle_coordinates: set[tuple[str, int]] = set()

    for line_number, record in enumerate(records, start=1):
        if not isinstance(record, dict):
            raise AcceptanceValidationError(f"line {line_number} must be a JSON object")
        kind = record.get("kind")
        if kind == "cold_start":
            _require_exact_keys(f"line {line_number}", record, COLD_START_FIELDS)
        elif kind == "cycle":
            _require_exact_keys(f"line {line_number}", record, CYCLE_FIELDS)
        else:
            raise AcceptanceValidationError(f"line {line_number} has an invalid kind")

        _validate_common(record, line_number, expected)
        host = record["host_platform"]

        if kind == "cold_start":
            ready_order = record["ready_order"]
            if ready_order not in READY_ORDERS:
                raise AcceptanceValidationError(
                    f"line {line_number} has an invalid ready_order"
                )
            pass_index = _require_int(
                f"line {line_number} pass_index", record["pass_index"], 1, 3
            )
            _require_int(
                f"line {line_number} countdown_ms",
                record["countdown_ms"],
                1,
                MAX_COUNTDOWN_MS,
            )
            _require_equal(
                f"line {line_number} result", record["result"], "countdown_reached"
            )
            coordinate = (host, ready_order, pass_index)
            if coordinate in cold_coordinates:
                raise AcceptanceValidationError(
                    f"line {line_number} duplicates cold-start coordinate {coordinate}"
                )
            cold_coordinates.add(coordinate)
        else:
            cycle_index = _require_int(
                f"line {line_number} cycle_index", record["cycle_index"], 1, 20
            )
            _require_equal(
                f"line {line_number} result", record["result"], "returned_to_lobby"
            )
            coordinate = (host, cycle_index)
            if coordinate in cycle_coordinates:
                raise AcceptanceValidationError(
                    f"line {line_number} duplicates cycle coordinate {coordinate}"
                )
            cycle_coordinates.add(coordinate)

    required_cold = {
        (host, ready_order, pass_index)
        for host in PLATFORMS
        for ready_order in READY_ORDERS
        for pass_index in range(1, 4)
    }
    required_cycles = {
        (host, cycle_index)
        for host in PLATFORMS
        for cycle_index in range(1, 21)
    }
    if cold_coordinates != required_cold:
        raise AcceptanceValidationError("cold-start matrix is incomplete or out of scope")
    if cycle_coordinates != required_cycles:
        raise AcceptanceValidationError("bidirectional 20-cycle matrix is incomplete")


def _object_without_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise AcceptanceValidationError(f"duplicate JSON field: {key}")
        value[key] = item
    return value


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise AcceptanceValidationError("cannot inspect evidence file") from error
    if size == 0 or size > MAX_EVIDENCE_BYTES:
        raise AcceptanceValidationError(
            f"evidence file must be 1..{MAX_EVIDENCE_BYTES} bytes"
        )
    try:
        contents = path.read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise AcceptanceValidationError("evidence must be readable UTF-8") from error

    records: list[dict[str, Any]] = []
    for line_number, line in enumerate(contents.splitlines(), start=1):
        encoded_size = len(line.encode("utf-8"))
        if encoded_size == 0 or encoded_size > MAX_RECORD_BYTES:
            raise AcceptanceValidationError(
                f"line {line_number} must be 1..{MAX_RECORD_BYTES} bytes"
            )
        try:
            record = json.loads(line, object_pairs_hook=_object_without_duplicate_keys)
        except AcceptanceValidationError:
            raise
        except (json.JSONDecodeError, ValueError) as error:
            raise AcceptanceValidationError(f"line {line_number} is not valid JSON") from error
        if not isinstance(record, dict):
            raise AcceptanceValidationError(f"line {line_number} must be a JSON object")
        records.append(record)
    return records


def _zero_cleanup() -> dict[str, int]:
    return {field: 0 for field in sorted(CLEANUP_FIELDS)}


def _synthetic_expected() -> ExpectedBuild:
    return ExpectedBuild(
        source_tag="spacewar-test-8",
        source_commit="1" * 40,
        compatibility_build_id="2" * 32,
        gameplay_content_hash="3" * 64,
        macos_archive_sha256="4" * 64,
        windows_archive_sha256="5" * 64,
    )


def _synthetic_common(host: str, expected: ExpectedBuild) -> dict[str, Any]:
    guest = "windows" if host == "macos" else "macos"
    return {
        "schema_version": SCHEMA_VERSION,
        "steam_app_id": STEAM_APP_ID,
        "host_platform": host,
        "guest_platform": guest,
        "host_source_tag": expected.source_tag,
        "guest_source_tag": expected.source_tag,
        "host_source_commit": expected.source_commit,
        "guest_source_commit": expected.source_commit,
        "host_compatibility_build_id": expected.compatibility_build_id,
        "guest_compatibility_build_id": expected.compatibility_build_id,
        "host_gameplay_content_hash": expected.gameplay_content_hash,
        "guest_gameplay_content_hash": expected.gameplay_content_hash,
        "host_archive_sha256": expected.archive_sha256(host),
        "guest_archive_sha256": expected.archive_sha256(guest),
        "host_cleanup": _zero_cleanup(),
        "guest_cleanup": _zero_cleanup(),
    }


def _synthetic_records(expected: ExpectedBuild) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for host in PLATFORMS:
        for ready_order in READY_ORDERS:
            for pass_index in range(1, 4):
                record = _synthetic_common(host, expected)
                record.update(
                    {
                        "kind": "cold_start",
                        "ready_order": ready_order,
                        "pass_index": pass_index,
                        "countdown_ms": MAX_COUNTDOWN_MS,
                        "result": "countdown_reached",
                    }
                )
                records.append(record)
        for cycle_index in range(1, 21):
            record = _synthetic_common(host, expected)
            record.update(
                {
                    "kind": "cycle",
                    "cycle_index": cycle_index,
                    "result": "returned_to_lobby",
                }
            )
            records.append(record)
    return records


class ValidatorSelfTests(unittest.TestCase):
    def setUp(self) -> None:
        self.expected = _synthetic_expected()
        self.records = _synthetic_records(self.expected)

    def assert_rejected(self, records: Sequence[dict[str, Any]]) -> None:
        with self.assertRaises(AcceptanceValidationError):
            validate_records(records, self.expected)

    def test_complete_campaign_passes(self) -> None:
        validate_records(self.records, self.expected)

    def test_missing_or_duplicate_coordinate_is_rejected(self) -> None:
        missing = copy.deepcopy(self.records[:-1])
        self.assert_rejected(missing)
        duplicate = copy.deepcopy(self.records)
        duplicate[-1] = copy.deepcopy(duplicate[-2])
        self.assert_rejected(duplicate)

    def test_countdown_over_thirty_seconds_is_rejected(self) -> None:
        records = copy.deepcopy(self.records)
        records[0]["countdown_ms"] = MAX_COUNTDOWN_MS + 1
        self.assert_rejected(records)

    def test_build_and_archive_mismatch_are_rejected(self) -> None:
        build_mismatch = copy.deepcopy(self.records)
        build_mismatch[0]["guest_compatibility_build_id"] = "a" * 32
        self.assert_rejected(build_mismatch)
        archive_mismatch = copy.deepcopy(self.records)
        archive_mismatch[0]["host_archive_sha256"] = "b" * 64
        self.assert_rejected(archive_mismatch)

    def test_nonzero_cleanup_count_is_rejected(self) -> None:
        records = copy.deepcopy(self.records)
        records[0]["guest_cleanup"]["auth_sessions"] = 1
        self.assert_rejected(records)

    def test_unknown_privacy_unsafe_field_is_rejected(self) -> None:
        records = copy.deepcopy(self.records)
        records[0]["steam_id"] = "not-permitted"
        self.assert_rejected(records)

    def test_jsonl_loader_rejects_duplicate_fields_and_accepts_fixture(self) -> None:
        with tempfile.TemporaryDirectory(prefix="afc-spacewar-acceptance-test-") as root:
            valid = Path(root) / "valid.jsonl"
            valid.write_text(
                "".join(
                    json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n"
                    for record in self.records
                ),
                encoding="utf-8",
            )
            validate_records(load_jsonl(valid), self.expected)

            duplicate = Path(root) / "duplicate.jsonl"
            duplicate.write_text('{"kind":"cycle","kind":"cycle"}\n', encoding="utf-8")
            with self.assertRaises(AcceptanceValidationError):
                load_jsonl(duplicate)


def _expected_from_arguments(arguments: argparse.Namespace) -> ExpectedBuild:
    return ExpectedBuild(
        source_tag=arguments.source_tag,
        source_commit=arguments.source_commit,
        compatibility_build_id=arguments.compatibility_build_id,
        gameplay_content_hash=arguments.gameplay_content_hash,
        macos_archive_sha256=arguments.macos_archive_sha256,
        windows_archive_sha256=arguments.windows_archive_sha256,
    )


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate", help="validate one JSONL campaign")
    validate.add_argument("record", type=Path)
    validate.add_argument("--source-tag", required=True)
    validate.add_argument("--source-commit", required=True)
    validate.add_argument("--compatibility-build-id", required=True)
    validate.add_argument("--gameplay-content-hash", required=True)
    validate.add_argument("--macos-archive-sha256", required=True)
    validate.add_argument("--windows-archive-sha256", required=True)

    self_test = subparsers.add_parser("self-test", help="run synthetic validator tests")
    self_test.add_argument("-v", "--verbose", action="count", default=0)
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    parsed = _build_parser().parse_args(arguments)
    if parsed.command == "self-test":
        verbosity = 1 + min(parsed.verbose, 1)
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ValidatorSelfTests)
        result = unittest.TextTestRunner(verbosity=verbosity).run(suite)
        return 0 if result.wasSuccessful() else 1

    try:
        expected = _expected_from_arguments(parsed)
        records = load_jsonl(parsed.record)
        validate_records(records, expected)
    except AcceptanceValidationError as error:
        print(f"Spacewar acceptance evidence rejected: {error}", file=sys.stderr)
        return 2

    print(
        json.dumps(
            {
                "valid": True,
                "schema_version": SCHEMA_VERSION,
                "cold_starts": EXPECTED_COLD_STARTS,
                "cycles": EXPECTED_CYCLES,
                "cleanup_counts": "all_zero",
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
