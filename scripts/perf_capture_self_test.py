#!/usr/bin/env python3
"""Focused tests for the graphical performance capture acceptance gate."""

from __future__ import annotations

import copy
import unittest

from run_graphical_perf import validate_host_environment, validate_result


def valid_four_bot_result() -> dict[str, object]:
    return {
        "schema_version": 6,
        "scenario": "FourBotStress",
        "run_id": "test",
        "warmup_seconds": 30.0,
        "requested_measurement_seconds": 300.0,
        "canonical_capture_eligible": True,
        "present_mode_policy": "AutoVsync",
        "uncapped_present_mode": False,
        "continuous_update_mode_valid": True,
        "requested_exit_code": 0,
        "fixture_valid": True,
        "fixture_invalid_reasons": [],
        "failure": "",
        "simulation_ready": True,
        "scene_instances_ready": True,
        "scene_root_readiness_non_vacuous": True,
        "fixture_counts_valid": True,
        "surface_present_invocation_observed": True,
        "render_evidence": "same_window_same_view_surface_texture_present_invoked",
        "initial_present_ack_count": 1,
        "final_present_ack_count": 1,
        "journal_continuity_valid": True,
        "event_continuity_valid": True,
        "journal_gap_ticks": 0,
        "event_overflow_delta": 0,
        "resource_stability_valid": True,
        "stale_owner_entities_peak": 0,
        "stale_owner_entities_end": 0,
        "frame_samples": 1_000,
        "frame_classification_sample_count": 1_000,
        "frame_classification_steady_samples": 990,
        "frame_classification_transition_samples": 0,
        "frame_classification_finalization_samples": 10,
        "frame_classification_gap_valid": True,
        "fixed_combat_fixture_mode": "fixed",
        "fixed_combat_fixture_arena_index": 5,
        "fixed_combat_fixture_arena_name": "Bumper Alley",
        "fixed_combat_fixture_authored_items": 4,
        "fixed_combat_fixture_authored_hazards": 3,
        "fixed_combat_fixture_public_hazard_markers": 3,
        "fixture_expected_fighters": 4,
        "fixture_observed_fighters": 4,
        "fixture_expected_combatant_bots": 4,
        "fixture_observed_combatant_bots": 4,
        "fixture_expected_items": 4,
        "fixture_observed_items": 4,
        "fixture_expected_hazard_markers": 3,
        "fixture_observed_hazard_markers": 3,
        "activity_valid": True,
        "owner_valid": True,
        "combat_activity_owner_valid": [True] * 4,
        "owner_workload_owner_valid": [True] * 4,
        "allocation_instrumentation": False,
        "local_timing_budget_pass": True,
        "performance_acceptance_status": "external_gpu_evidence_required",
        "gpu_completion_measured": False,
        "external_gpu_evidence_status": "required_not_collected",
        "rss_growth_acceptance_status": "diagnostic_not_gated_for_scenario",
    }


def valid_map_cycle_result() -> dict[str, object]:
    result = valid_four_bot_result()
    result.update(
        {
            "scenario": "MapCycle100",
            "fixed_combat_fixture_mode": "nonfixed_map_cycle",
            "fixed_combat_fixture_arena_index": None,
            "frame_classification_steady_samples": 790,
            "frame_classification_transition_samples": 200,
            "frame_classification_finalization_samples": 10,
            "map_switches": 100,
            "map_switch_samples": 100,
            "map_switch_samples_valid": True,
            "map_cycle_resource_checkpoint_observed": True,
            "map_cycle_preload_required": True,
            "map_cycle_preload_ready": True,
            "map_cycle_preload_asset_count": 101,
            "map_cycle_preload_folders": ["arena", "backgrounds", "music/bgm"],
            "map_warm_precycle_valid": True,
            "map_warm_precycle_present_ack_count": 10,
            "map_measured_present_ack_count": 100,
            "aligned_cycle_checkpoint_count": 11,
            "aligned_rss_growth_acceptance_status": "passed",
            "rss_growth_acceptance_status": "passed",
        }
    )
    return result


def valid_soak_result() -> dict[str, object]:
    result = valid_four_bot_result()
    result.update(
        {
            "scenario": "Soak10Minutes",
            "requested_measurement_seconds": 600.0,
            "rss_growth_acceptance_status": "passed",
        }
    )
    return result


def as_valid_alloc(result: dict[str, object]) -> dict[str, object]:
    allocated = copy.deepcopy(result)
    allocated.update(
        {
            "allocation_instrumentation": True,
            "allocation_measurement_status": "available",
            "allocations": 10,
            "allocated_bytes": 1_024,
            "live_bytes_start": 100,
            "live_bytes_end": 100,
            "live_bytes_delta": 0,
            "peak_live_bytes": 200,
            # Timing from an instrumented allocator build is deliberately not
            # authoritative and must not block valid allocation evidence.
            "local_timing_budget_pass": False,
            "performance_acceptance_status": "external_gpu_evidence_required",
            "external_gpu_evidence_status": "required_not_collected",
        }
    )
    if allocated["scenario"] == "MapCycle100":
        allocated["aligned_live_growth_evidence_status"] = "available_passed"
    if allocated["scenario"] == "Soak10Minutes":
        allocated["live_growth_evidence_status"] = "available_passed"
    return allocated


def mac_host(
    *,
    power_source: str = "AC Power",
    binary_file: str = "Mach-O 64-bit executable arm64",
) -> dict[str, object]:
    return {
        "os_name": "Darwin",
        "macos": "ProductName: macOS",
        "apple_silicon_capable": "1",
        "binary_sha256": "abc",
        "binary_file": binary_file,
        "power": f"Now drawing from '{power_source}'",
    }


class ValidationTests(unittest.TestCase):
    def validate(self, result: dict[str, object], **overrides: object) -> list[str]:
        arguments = {
            "scenario": "FourBotStress",
            "run_id": "test",
            "kind": "timing",
            "actual_exit_code": 0,
            "timed_out": False,
        }
        arguments.update(overrides)
        return validate_result(result, **arguments)  # type: ignore[arg-type]

    def test_canonical_timing_result_passes(self) -> None:
        self.assertEqual(self.validate(valid_four_bot_result()), [])

    def test_exploratory_or_invalid_result_is_rejected(self) -> None:
        result = valid_four_bot_result()
        result["canonical_capture_eligible"] = False
        result["fixture_valid"] = False
        result["fixture_invalid_reasons"] = ["combat_activity_invalid"]
        errors = self.validate(result)
        self.assertTrue(any("exploratory" in error for error in errors))
        self.assertTrue(any("fixture_valid" in error for error in errors))
        self.assertTrue(any("invalid reasons" in error for error in errors))
        self.assertTrue(any("evidence order" in error for error in errors))

    def test_wrong_arena_or_process_status_is_rejected(self) -> None:
        result = copy.deepcopy(valid_four_bot_result())
        result["fixed_combat_fixture_arena_index"] = 3
        errors = self.validate(result, actual_exit_code=1)
        self.assertTrue(any("process exit" in error for error in errors))
        self.assertTrue(any("arena index" in error for error in errors))

    def test_map_cycle_requires_warm_precycle_and_aligned_evidence(self) -> None:
        result = valid_map_cycle_result()
        self.assertEqual(self.validate(result, scenario="MapCycle100"), [])

        result["map_warm_precycle_valid"] = False
        result["map_warm_precycle_present_ack_count"] = 9
        result["map_measured_present_ack_count"] = 99
        result["aligned_cycle_checkpoint_count"] = 10
        errors = self.validate(result, scenario="MapCycle100")
        self.assertTrue(any("warm precycle is invalid" in error for error in errors))
        self.assertTrue(any("exactly 10 presents" in error for error in errors))
        self.assertTrue(any("exactly 100 presents" in error for error in errors))
        self.assertTrue(any("11 aligned checkpoints" in error for error in errors))

    def test_map_cycle_requires_completed_recursive_preload(self) -> None:
        result = valid_map_cycle_result()
        result["map_cycle_preload_ready"] = False
        errors = self.validate(result, scenario="MapCycle100")
        self.assertTrue(any("preload did not finish" in error for error in errors))

    def test_presentation_policy_and_nonvacuous_scene_roots_are_required(self) -> None:
        result = valid_four_bot_result()
        result["present_mode_policy"] = "AutoNoVsync"
        result["uncapped_present_mode"] = True
        result["scene_root_readiness_non_vacuous"] = False
        result["initial_present_ack_count"] = 0
        result["final_present_ack_count"] = 0
        errors = self.validate(result)
        self.assertTrue(any("AutoVsync" in error for error in errors))
        self.assertTrue(any("uncapped" in error for error in errors))
        self.assertTrue(any("vacuous" in error for error in errors))
        self.assertTrue(any("initial present" in error for error in errors))
        self.assertTrue(any("final present" in error for error in errors))

    def test_frame_classification_must_be_exact_and_gap_free(self) -> None:
        result = valid_four_bot_result()
        result["frame_classification_steady_samples"] = 989
        result["frame_classification_gap_valid"] = False
        errors = self.validate(result)
        self.assertTrue(any("sum exactly" in error for error in errors))
        self.assertTrue(any("gap or overlap" in error for error in errors))

    def test_noncontinuous_event_loop_is_rejected(self) -> None:
        result = valid_four_bot_result()
        result["continuous_update_mode_valid"] = False
        errors = self.validate(result)
        self.assertTrue(any("event loop" in error for error in errors))

    def test_gpu_completion_claim_or_wrong_external_status_is_rejected(self) -> None:
        result = valid_four_bot_result()
        result["gpu_completion_measured"] = True
        result["external_gpu_evidence_status"] = "available"
        errors = self.validate(result)
        self.assertTrue(any("must not claim" in error for error in errors))
        self.assertTrue(any("external GPU evidence status" in error for error in errors))

    def test_map_rss_failure_precedes_external_gpu_status(self) -> None:
        result = valid_map_cycle_result()
        result["aligned_rss_growth_acceptance_status"] = "failed"
        errors = self.validate(result, scenario="MapCycle100")
        self.assertTrue(any("aligned map RSS" in error for error in errors))
        self.assertTrue(
            any("expected rss_growth_evidence_failed" in error for error in errors)
        )

    def test_timing_failure_precedes_rss_and_external_gpu_status(self) -> None:
        result = valid_map_cycle_result()
        result["local_timing_budget_pass"] = False
        result["aligned_rss_growth_acceptance_status"] = "failed"
        errors = self.validate(result, scenario="MapCycle100")
        self.assertTrue(any("local timing budget failed" in error for error in errors))
        self.assertTrue(
            any("expected local_timing_budget_failed" in error for error in errors)
        )

    def test_map_and_soak_alloc_builds_require_live_plateau(self) -> None:
        map_result = as_valid_alloc(valid_map_cycle_result())
        self.assertEqual(
            self.validate(map_result, scenario="MapCycle100", kind="alloc"),
            [],
        )
        map_result["aligned_live_growth_evidence_status"] = "available_failed"
        map_errors = self.validate(map_result, scenario="MapCycle100", kind="alloc")
        self.assertTrue(any("aligned map live-byte" in error for error in map_errors))
        self.assertTrue(
            any("expected live_growth_evidence_failed" in error for error in map_errors)
        )

        soak_result = as_valid_alloc(valid_soak_result())
        self.assertEqual(
            self.validate(soak_result, scenario="Soak10Minutes", kind="alloc"),
            [],
        )
        soak_result["live_growth_evidence_status"] = "available_failed"
        soak_errors = self.validate(
            soak_result, scenario="Soak10Minutes", kind="alloc"
        )
        self.assertTrue(any("live-byte plateau" in error for error in soak_errors))

    def test_macos_host_must_remain_on_ac_power(self) -> None:
        self.assertEqual(validate_host_environment(mac_host(), mac_host()), [])

        battery_errors = validate_host_environment(
            mac_host(power_source="Battery Power"),
            mac_host(power_source="Battery Power"),
        )
        self.assertTrue(any("start on AC Power" in error for error in battery_errors))
        self.assertTrue(any("end on AC Power" in error for error in battery_errors))

        changed_errors = validate_host_environment(
            mac_host(),
            mac_host(power_source="Battery Power"),
        )
        self.assertTrue(any("changed during capture" in error for error in changed_errors))

    def test_apple_silicon_host_requires_native_arm64_binary(self) -> None:
        errors = validate_host_environment(
            mac_host(binary_file="Mach-O 64-bit executable x86_64"),
            mac_host(binary_file="Mach-O 64-bit executable x86_64"),
        )
        self.assertTrue(any("not native arm64 before" in error for error in errors))
        self.assertTrue(any("not native arm64 after" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
