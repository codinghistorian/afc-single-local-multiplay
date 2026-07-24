#!/usr/bin/env python3
"""Run and strictly validate the canonical graphical performance matrix."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import queue
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
RESULT_MARKER = "AFC_PERF_RESULT "
RESULT_SCHEMA_VERSION = 6
CAPTURE_SCHEMA_VERSION = 2
RENDER_EVIDENCE = "same_window_same_view_surface_texture_present_invoked"
CANONICAL_PRESENT_MODE_POLICY = "AutoVsync"
EXTERNAL_GPU_REQUIRED_STATUS = "external_gpu_evidence_required"

# Schema-v6 result field names are intentionally centralized here. The Rust
# harness and this runner are developed together, but keeping the wire names in
# one place makes an in-flight schema reconciliation a small, auditable change.
V6_FIELDS = {
    "present_mode_policy": "present_mode_policy",
    "uncapped_present_mode": "uncapped_present_mode",
    "scene_root_readiness_non_vacuous": "scene_root_readiness_non_vacuous",
    "classified_frames": "frame_classification_sample_count",
    "steady_frames": "frame_classification_steady_samples",
    "transition_frames": "frame_classification_transition_samples",
    "finalization_frames": "frame_classification_finalization_samples",
    "frame_classification_gap_valid": "frame_classification_gap_valid",
    "initial_present_acks": "initial_present_ack_count",
    "final_present_acks": "final_present_ack_count",
    "map_warm_precycle_valid": "map_warm_precycle_valid",
    "map_warm_precycle_present_acks": "map_warm_precycle_present_ack_count",
    "map_measured_present_acks": "map_measured_present_ack_count",
    "map_aligned_cycle_checkpoints": "aligned_cycle_checkpoint_count",
    "map_aligned_rss_status": "aligned_rss_growth_acceptance_status",
    "map_aligned_live_status": "aligned_live_growth_evidence_status",
}
SCENARIOS = {
    "FourBotStress": {"seconds": 300.0, "runs": 3},
    "MapCycle100": {"seconds": 300.0, "runs": 3},
    "Soak10Minutes": {"seconds": 600.0, "runs": 1},
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def host_metadata(binary: Path, binary_sha256: str, kind: str) -> dict[str, Any]:
    def output(command: list[str]) -> str | None:
        try:
            return subprocess.run(
                command,
                check=False,
                capture_output=True,
                text=True,
                timeout=15,
            ).stdout.strip() or None
        except (OSError, subprocess.TimeoutExpired):
            return None

    return {
        "capture_schema_version": CAPTURE_SCHEMA_VERSION,
        "captured_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "kind": kind,
        "binary": str(binary),
        "binary_sha256": binary_sha256,
        "os_name": platform.system(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "kernel_machine": output(["uname", "-m"]),
        "binary_file": output(["file", "-b", str(binary)]),
        "python_rosetta_translated": output(["sysctl", "-in", "sysctl.proc_translated"])
        if sys.platform == "darwin"
        else None,
        "macos": output(["sw_vers"]) if sys.platform == "darwin" else None,
        "apple_silicon_capable": output(["sysctl", "-in", "hw.optional.arm64"])
        if sys.platform == "darwin"
        else None,
        "hardware": output(["system_profiler", "SPHardwareDataType"])
        if sys.platform == "darwin"
        else None,
        "power": output(["pmset", "-g", "batt"])
        if sys.platform == "darwin"
        else None,
        "git_head": output(["git", "rev-parse", "HEAD"]),
        "git_status_porcelain": output(["git", "status", "--short"]),
    }


def macos_power_source(metadata: dict[str, Any]) -> str | None:
    power = metadata.get("power")
    if not isinstance(power, str):
        return None
    if "AC Power" in power:
        return "AC Power"
    if "Battery Power" in power:
        return "Battery Power"
    return None


def is_macos_host(metadata: dict[str, Any]) -> bool:
    return metadata.get("os_name") == "Darwin" or metadata.get("macos") is not None


def apple_silicon_detected(metadata: dict[str, Any]) -> bool:
    if metadata.get("apple_silicon_capable") == "1":
        return True
    hardware = metadata.get("hardware")
    return isinstance(hardware, str) and "Chip: Apple" in hardware


def validate_host_environment(
    before: dict[str, Any], after: dict[str, Any]
) -> list[str]:
    """Fail closed on per-run host conditions that can invalidate a baseline."""

    errors: list[str] = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            errors.append(message)

    require(bool(before), "per-run host metadata before capture is missing")
    require(bool(after), "per-run host metadata after capture is missing")
    if not before or not after:
        return errors

    require(
        before.get("binary_sha256") == after.get("binary_sha256"),
        "binary hash changed between per-run host snapshots",
    )
    require(
        before.get("binary_file") == after.get("binary_file"),
        "binary architecture changed between per-run host snapshots",
    )

    if is_macos_host(before) or is_macos_host(after):
        before_power = macos_power_source(before)
        after_power = macos_power_source(after)
        require(before_power is not None, "macOS power source before capture is unavailable")
        require(after_power is not None, "macOS power source after capture is unavailable")
        require(
            before_power == after_power,
            "macOS power source changed during capture",
        )
        require(before_power == "AC Power", "canonical macOS capture did not start on AC Power")
        require(after_power == "AC Power", "canonical macOS capture did not end on AC Power")

        if apple_silicon_detected(before) or apple_silicon_detected(after):
            for edge, metadata in (("before", before), ("after", after)):
                binary_file = metadata.get("binary_file")
                require(
                    isinstance(binary_file, str)
                    and "Mach-O 64-bit executable arm64" in binary_file,
                    f"Apple Silicon capture binary was not native arm64 {edge} capture",
                )

    return errors


def terminate_process_group(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def stream_process(
    command: list[str], env: dict[str, str], log_path: Path, timeout_seconds: float
) -> tuple[int, bool, list[dict[str, Any]]]:
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        start_new_session=True,
    )
    assert process.stdout is not None
    lines: queue.Queue[str | None] = queue.Queue()

    def read_output() -> None:
        for line in process.stdout:
            lines.put(line)
        lines.put(None)

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    deadline = time.monotonic() + timeout_seconds
    timed_out = False
    results: list[dict[str, Any]] = []
    with log_path.open("w", encoding="utf-8") as log:
        while True:
            if time.monotonic() >= deadline and process.poll() is None:
                timed_out = True
                terminate_process_group(process)
            try:
                line = lines.get(timeout=1)
            except queue.Empty:
                continue
            if line is None:
                break
            sys.stdout.write(line)
            sys.stdout.flush()
            log.write(line)
            log.flush()
            marker = line.find(RESULT_MARKER)
            if marker >= 0:
                payload = line[marker + len(RESULT_MARKER) :].strip()
                try:
                    results.append(json.loads(payload))
                except json.JSONDecodeError as error:
                    results.append({"_parse_error": str(error), "_payload": payload})
        exit_code = process.wait()
        log.write(f"CAPTURE_PROCESS_EXIT={exit_code}\n")
        log.write(f"CAPTURE_TIMED_OUT={str(timed_out).lower()}\n")
    return exit_code, timed_out, results


def expected_performance_acceptance_status(
    result: dict[str, Any], *, scenario: str, kind: str
) -> str:
    """Mirror the schema-v6 fail-closed evidence ordering.

    Timing is authoritative only for an uninstrumented timing build. RSS and
    live-byte gates are scenario-specific, and external GPU evidence is requested
    only after every applicable local gate has passed.
    """

    if result.get("fixture_valid") is not True:
        return "fixture_invalid"
    if result.get("canonical_capture_eligible") is not True:
        return "exploratory_only_noncanonical_configuration"
    if kind == "timing" and result.get("local_timing_budget_pass") is not True:
        return "local_timing_budget_failed"

    rss_pass = True
    if scenario == "MapCycle100":
        rss_pass = (
            result.get(V6_FIELDS["map_aligned_rss_status"]) == "passed"
        )
    elif scenario == "Soak10Minutes":
        rss_pass = result.get("rss_growth_acceptance_status") == "passed"
    if not rss_pass:
        return "rss_growth_evidence_failed"

    live_pass = True
    if kind == "alloc" and scenario == "MapCycle100":
        live_pass = (
            result.get(V6_FIELDS["map_aligned_live_status"])
            == "available_passed"
        )
    elif kind == "alloc" and scenario == "Soak10Minutes":
        live_pass = result.get("live_growth_evidence_status") == "available_passed"
    if not live_pass:
        return "live_growth_evidence_failed"

    return EXTERNAL_GPU_REQUIRED_STATUS


def validate_result(
    result: dict[str, Any],
    *,
    scenario: str,
    run_id: str,
    kind: str,
    actual_exit_code: int,
    timed_out: bool,
) -> list[str]:
    errors: list[str] = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            errors.append(message)

    expected_seconds = SCENARIOS[scenario]["seconds"]
    require(not timed_out, "process timed out")
    require(actual_exit_code == 0, f"actual process exit was {actual_exit_code}, expected 0")
    require(result.get("schema_version") == RESULT_SCHEMA_VERSION, "unexpected result schema")
    require(result.get("scenario") == scenario, "scenario mismatch")
    require(result.get("run_id") == run_id, "run_id mismatch")
    require(result.get("warmup_seconds") == 30.0, "canonical warmup was not 30 seconds")
    require(
        result.get("requested_measurement_seconds") == expected_seconds,
        "canonical measurement duration mismatch",
    )
    require(result.get("canonical_capture_eligible") is True, "capture is exploratory-only")
    require(
        result.get(V6_FIELDS["present_mode_policy"]) == CANONICAL_PRESENT_MODE_POLICY,
        "canonical present mode policy is not AutoVsync",
    )
    require(
        result.get(V6_FIELDS["uncapped_present_mode"]) is False,
        "canonical capture unexpectedly uses an uncapped present mode",
    )
    require(
        result.get("continuous_update_mode_valid") is True,
        "profiling event loop is not continuous",
    )
    require(result.get("requested_exit_code") == 0, "harness requested a failure exit")
    require(result.get("fixture_valid") is True, "fixture_valid is not true")
    require(result.get("fixture_invalid_reasons") == [], "fixture has invalid reasons")
    require(result.get("failure") == "", "primary fixture failure is non-empty")
    require(result.get("simulation_ready") is True, "simulation was not ready")
    require(result.get("scene_instances_ready") is True, "arena scene instances were not ready")
    require(
        result.get(V6_FIELDS["scene_root_readiness_non_vacuous"]) is True,
        "arena scene-root readiness evidence is vacuous",
    )
    require(result.get("fixture_counts_valid") is True, "fixture counts are invalid")
    require(
        result.get("surface_present_invocation_observed") is True,
        "surface present invocation was not observed",
    )
    require(result.get("render_evidence") == RENDER_EVIDENCE, "render evidence mismatch")
    require(result.get("journal_continuity_valid") is True, "journal continuity failed")
    require(result.get("event_continuity_valid") is True, "event continuity failed")
    require(result.get("journal_gap_ticks") == 0, "journal gaps were observed")
    require(result.get("event_overflow_delta") == 0, "event overflow changed")
    require(result.get("resource_stability_valid") is True, "resource stability failed")
    require(result.get("stale_owner_entities_peak") == 0, "stale owner peak was nonzero")
    require(result.get("stale_owner_entities_end") == 0, "stale owners remained at end")
    require(
        result.get(V6_FIELDS["initial_present_acks"]) == 1,
        "initial present acknowledgement count changed",
    )
    require(
        result.get(V6_FIELDS["final_present_acks"]) == 1,
        "final present acknowledgement count changed",
    )

    frame_samples = result.get("frame_samples")
    classified_frames = result.get(V6_FIELDS["classified_frames"])
    frame_classes = [
        result.get(V6_FIELDS["steady_frames"]),
        result.get(V6_FIELDS["transition_frames"]),
        result.get(V6_FIELDS["finalization_frames"]),
    ]
    require(
        isinstance(frame_samples, int) and not isinstance(frame_samples, bool) and frame_samples > 0,
        "measured frame sample count is not positive",
    )
    require(
        isinstance(classified_frames, int)
        and not isinstance(classified_frames, bool)
        and classified_frames >= 0,
        "classified frame sample count is invalid",
    )
    require(
        all(
            isinstance(value, int) and not isinstance(value, bool) and value >= 0
            for value in frame_classes
        ),
        "one or more frame-class sample counts are invalid",
    )
    if (
        isinstance(frame_samples, int)
        and not isinstance(frame_samples, bool)
        and isinstance(classified_frames, int)
        and not isinstance(classified_frames, bool)
        and all(
            isinstance(value, int) and not isinstance(value, bool)
            for value in frame_classes
        )
    ):
        require(
            sum(frame_classes) == classified_frames == frame_samples,
            "frame classifications do not sum exactly to measured frames",
        )
    require(
        result.get(V6_FIELDS["frame_classification_gap_valid"]) is True,
        "frame classification contains a gap or overlap",
    )

    if scenario in ("FourBotStress", "Soak10Minutes"):
        require(result.get("fixed_combat_fixture_mode") == "fixed", "combat arena is not fixed")
        require(result.get("fixed_combat_fixture_arena_index") == 5, "combat arena index changed")
        require(result.get("fixed_combat_fixture_arena_name") == "Bumper Alley", "combat arena changed")
        require(result.get("fixed_combat_fixture_authored_items") == 4, "authored item count changed")
        require(result.get("fixed_combat_fixture_authored_hazards") == 3, "authored hazard count changed")
        require(result.get("fixed_combat_fixture_public_hazard_markers") == 3, "public hazard count changed")
        for key, expected in (
            ("fixture_expected_fighters", 4),
            ("fixture_observed_fighters", 4),
            ("fixture_expected_combatant_bots", 4),
            ("fixture_observed_combatant_bots", 4),
            ("fixture_expected_items", 4),
            ("fixture_observed_items", 4),
            ("fixture_expected_hazard_markers", 3),
            ("fixture_observed_hazard_markers", 3),
        ):
            require(result.get(key) == expected, f"{key} changed")
        require(result.get("activity_valid") is True, "combat activity failed")
        require(result.get("owner_valid") is True, "owner workload failed")
        require(result.get("combat_activity_owner_valid") == [True] * 4, "per-owner activity failed")
        require(result.get("owner_workload_owner_valid") == [True] * 4, "per-owner workload failed")
    else:
        require(result.get("fixed_combat_fixture_mode") == "nonfixed_map_cycle", "map-cycle fixture mode changed")
        require(result.get("fixed_combat_fixture_arena_index") is None, "map cycle reported a fixed arena")
        require(result.get("map_switches") == 100, "map cycle did not complete 100 switches")
        require(result.get("map_switch_samples") == 100, "map cycle did not retain 100 samples")
        require(result.get("map_switch_samples_valid") is True, "map switch samples are invalid")
        require(result.get("map_cycle_preload_required") is True, "map preload is not required")
        require(result.get("map_cycle_preload_ready") is True, "map preload did not finish")
        require(
            isinstance(result.get("map_cycle_preload_asset_count"), int)
            and result["map_cycle_preload_asset_count"] == 101,
            "map preload asset catalog changed",
        )
        require(
            result.get("map_cycle_preload_folders")
            == ["arena", "backgrounds", "music/bgm"],
            "map preload folder contract changed",
        )
        require(
            result.get("map_cycle_resource_checkpoint_observed") is True,
            "map first-cycle resource checkpoint is missing",
        )
        require(
            result.get(V6_FIELDS["map_warm_precycle_valid"]) is True,
            "map warm precycle is invalid",
        )
        require(
            result.get(V6_FIELDS["map_warm_precycle_present_acks"]) == 10,
            "map warm precycle did not acknowledge exactly 10 presents",
        )
        require(
            result.get(V6_FIELDS["map_measured_present_acks"]) == 100,
            "map measurement did not acknowledge exactly 100 presents",
        )
        require(
            result.get(V6_FIELDS["map_aligned_cycle_checkpoints"]) == 11,
            "map cycle did not retain exactly 11 aligned checkpoints",
        )
        require(
            result.get(V6_FIELDS["map_aligned_rss_status"]) == "passed",
            "aligned map RSS growth evidence failed",
        )

    instrumented = result.get("allocation_instrumentation") is True
    if kind == "timing":
        require(not instrumented, "timing build unexpectedly instruments allocations")
        require(result.get("local_timing_budget_pass") is True, "local timing budget failed")
    else:
        require(instrumented, "allocation build did not instrument allocations")
        require(result.get("allocation_measurement_status") == "available", "allocation evidence is unavailable")
        for key in (
            "allocations",
            "allocated_bytes",
            "live_bytes_start",
            "live_bytes_end",
            "live_bytes_delta",
            "peak_live_bytes",
        ):
            require(isinstance(result.get(key), int), f"{key} is not an integer")

    if scenario == "FourBotStress":
        require(
            result.get("rss_growth_acceptance_status") == "diagnostic_not_gated_for_scenario",
            "FourBotStress RSS status changed",
        )
    elif scenario == "Soak10Minutes":
        require(result.get("rss_growth_acceptance_status") == "passed", "RSS growth evidence failed")
    if scenario == "MapCycle100" and kind == "alloc":
        require(
            result.get(V6_FIELDS["map_aligned_live_status"]) == "available_passed",
            "aligned map live-byte growth evidence failed",
        )
    elif scenario == "Soak10Minutes" and kind == "alloc":
        require(
            result.get("live_growth_evidence_status") == "available_passed",
            "live-byte plateau failed",
        )

    expected_status = expected_performance_acceptance_status(
        result, scenario=scenario, kind=kind
    )
    require(
        result.get("performance_acceptance_status") == expected_status,
        f"performance acceptance status did not follow evidence order; expected {expected_status}",
    )
    require(
        result.get("gpu_completion_measured") is False,
        "local harness must not claim asynchronous GPU completion",
    )
    expected_external_gpu_status = (
        "required_not_collected"
        if expected_status == EXTERNAL_GPU_REQUIRED_STATUS
        else {
            "fixture_invalid": "not_evaluated_fixture_invalid",
            "exploratory_only_noncanonical_configuration": "not_evaluated_exploratory_only",
            "local_timing_budget_failed": "not_evaluated_local_timing_budget_failed",
            "rss_growth_evidence_failed": "not_evaluated_rss_growth_evidence_failed",
            "live_growth_evidence_failed": "not_evaluated_live_growth_evidence_failed",
        }[expected_status]
    )
    require(
        result.get("external_gpu_evidence_status") == expected_external_gpu_status,
        "external GPU evidence status did not follow local acceptance state",
    )

    return errors


def capture_run(
    *,
    binary: Path,
    expected_sha256: str,
    output_dir: Path,
    scenario: str,
    run_id: str,
    kind: str,
    use_caffeinate: bool,
) -> dict[str, Any]:
    if sha256_file(binary) != expected_sha256:
        raise RuntimeError("profiling executable changed after matrix start")
    host_before = host_metadata(binary, expected_sha256, kind)
    (output_dir / f"{run_id}.host-before.json").write_text(
        json.dumps(host_before, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = [str(binary)]
    if use_caffeinate and sys.platform == "darwin" and shutil.which("caffeinate"):
        command = ["caffeinate", "-dimsu", "--", *command]
    env = os.environ.copy()
    env.update(
        {
            "BEVY_ASSET_ROOT": str(ROOT),
            "AFC_PERF_SCENARIO": scenario,
            "AFC_PERF_RUN_ID": run_id,
        }
    )
    for override in (
        "AFC_PERF_WARMUP_SECONDS",
        "AFC_PERF_MEASUREMENT_SECONDS",
        "AFC_PERF_SEED",
        "AFC_PERF_UNCAPPED",
    ):
        env.pop(override, None)
    log_path = output_dir / f"{run_id}.log"
    timeout_seconds = 30.0 + SCENARIOS[scenario]["seconds"] + 120.0
    exit_code, timed_out, results = stream_process(command, env, log_path, timeout_seconds)
    ending_sha256 = sha256_file(binary)
    host_after = host_metadata(binary, ending_sha256, kind)
    (output_dir / f"{run_id}.host-after.json").write_text(
        json.dumps(host_after, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    if len(results) == 1 and "_parse_error" not in results[0]:
        result = results[0]
        errors = validate_result(
            result,
            scenario=scenario,
            run_id=run_id,
            kind=kind,
            actual_exit_code=exit_code,
            timed_out=timed_out,
        )
    else:
        result = results[0] if len(results) == 1 else None
        errors = [f"expected exactly one parseable AFC_PERF_RESULT, observed {len(results)}"]
        if timed_out:
            errors.append("process timed out")
        if exit_code != 0:
            errors.append(f"actual process exit was {exit_code}, expected 0")
    errors.extend(validate_host_environment(host_before, host_after))
    if ending_sha256 != expected_sha256:
        errors.append("profiling executable changed during capture")
    record = {
        "capture_schema_version": CAPTURE_SCHEMA_VERSION,
        "scenario": scenario,
        "run_id": run_id,
        "kind": kind,
        "binary_sha256": expected_sha256,
        "actual_exit_code": exit_code,
        "timed_out": timed_out,
        "result_count": len(results),
        "accepted": not errors,
        "errors": errors,
        "result": result,
        "host_before": host_before,
        "host_after": host_after,
    }
    (output_dir / f"{run_id}.capture.json").write_text(
        json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return record


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--kind", required=True, choices=("timing", "alloc"))
    parser.add_argument("--scenario", action="append", choices=tuple(SCENARIOS))
    parser.add_argument("--runs", type=int, help="override run count for each selected scenario")
    parser.add_argument("--run-prefix", default=dt.datetime.now().strftime("%Y%m%d-%H%M%S"))
    parser.add_argument("--no-caffeinate", action="store_true")
    parser.add_argument("--continue-on-failure", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"profiling executable does not exist: {binary}")
    if args.runs is not None and args.runs <= 0:
        raise SystemExit("--runs must be positive")
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    binary_sha256 = sha256_file(binary)
    (output_dir / "host.json").write_text(
        json.dumps(host_metadata(binary, binary_sha256, args.kind), indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )
    selected = args.scenario or list(SCENARIOS)
    records: list[dict[str, Any]] = []
    for scenario in selected:
        runs = args.runs if args.runs is not None else int(SCENARIOS[scenario]["runs"])
        for index in range(1, runs + 1):
            slug = scenario.lower()
            run_id = f"{args.run_prefix}-{args.kind}-{slug}-{index:02d}"
            print(f"AFC_CAPTURE_BEGIN scenario={scenario} run_id={run_id}", flush=True)
            record = capture_run(
                binary=binary,
                expected_sha256=binary_sha256,
                output_dir=output_dir,
                scenario=scenario,
                run_id=run_id,
                kind=args.kind,
                use_caffeinate=not args.no_caffeinate,
            )
            records.append(record)
            (output_dir / "matrix.json").write_text(
                json.dumps(records, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            print(
                f"AFC_CAPTURE_END run_id={run_id} accepted={str(record['accepted']).lower()} "
                f"errors={record['errors']}",
                flush=True,
            )
            if not record["accepted"] and not args.continue_on_failure:
                return 1
    return 0 if all(record["accepted"] for record in records) else 1


if __name__ == "__main__":
    raise SystemExit(main())
