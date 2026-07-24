"""Synthetic tests for the AFC release packaging tool.

These tests deliberately use tiny hand-built executable headers and fake Steam API
libraries.  They require neither Cargo nor proprietary Steamworks files.
"""

from __future__ import annotations

import copy
import json
import os
import shutil
import struct
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from typing import Any
from unittest import mock

import release_lib


TEST_COMMIT = "a" * 40
TEST_APP_ID = 1_234_567
TEST_RELEASE_LABEL = "steam-rc.7+abcdef123456"
TEST_DEPOT_IDS = {
    "windows-x86_64": 1_234_568,
    "linux-x86_64": 1_234_569,
    "macos-universal": 1_234_570,
}


def fixture_identity(**updates: Any) -> dict[str, Any]:
    identity: dict[str, Any] = {
        "schema_version": 1,
        "product_name": "Animal Fighter Club",
        "package_name": "ffc-prototype",
        "product_version": "0.1.0",
        "release_label": TEST_RELEASE_LABEL,
        "shipping": True,
        "steam_app_id": TEST_APP_ID,
        "steam_depot_build_id": None,
        "protocol_version": 1,
        "simulation_version": 5,
        "rng_scheme_version": 1,
        "replay_format_version": 1,
        "snapshot_schema_version": 2,
        "compatibility_build_id": "01" * 16,
        "gameplay_content_hash": "23" * 32,
    }
    identity.update(updates)
    return identity


def write_pe_x86_64(path: Path) -> None:
    contents = bytearray(512)
    contents[:2] = b"MZ"
    struct.pack_into("<I", contents, 0x3C, 0x80)
    contents[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<H", contents, 0x84, 0x8664)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)


def write_elf_x86_64(path: Path) -> None:
    contents = bytearray(128)
    contents[:4] = b"\x7fELF"
    contents[4] = 2
    contents[5] = 1
    struct.pack_into("<H", contents, 18, 62)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)


def write_macho_universal(path: Path) -> None:
    records = []
    offset = 4096
    for cpu_type in (0x0100_0007, 0x0100_000C):
        records.append(struct.pack(">IIIII", cpu_type, 0, offset, 1, 0))
        offset += 1
    contents = struct.pack(">II", 0xCAFE_BABE, len(records)) + b"".join(records)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)


def create_binary_inputs(root: Path, platform: str, policy: dict[str, Any]) -> tuple[Path, Path]:
    platform_policy = policy["platforms"][platform]
    binary = root / "inputs" / platform / Path(platform_policy["binary_name"]).name
    redistributable = (
        root / "inputs" / platform / platform_policy["redistributable_name"]
    )
    if platform == "windows-x86_64":
        write_pe_x86_64(binary)
        write_pe_x86_64(redistributable)
    elif platform == "linux-x86_64":
        write_elf_x86_64(binary)
        write_elf_x86_64(redistributable)
    else:
        write_macho_universal(binary)
        write_macho_universal(redistributable)
    return binary, redistributable


def create_source_assets(root: Path, policy: dict[str, Any]) -> list[str]:
    assets = [
        *policy["required_runtime_assets"],
        "assets/characters/kenney_cube_pets/License.txt",
        "assets/characters/kenney_cube_pets/animal-cat.glb",
    ]
    for relative in assets:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.suffix == ".vdf":
            path.write_text('"Synthetic"\n{\n    "Value" "1"\n}\n', encoding="utf-8")
        elif path.suffix == ".glb":
            path.write_bytes(b"glTF-synthetic")
        else:
            path.write_text("Synthetic CC0 fixture\n", encoding="utf-8")
    for relative in policy["embedded_build_only_paths"]:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("(embedded: true)\n", encoding="utf-8")
    return sorted(assets)


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="afc-release-self-test-")
        self.root = Path(self.temporary.name)
        self.repository_root = release_lib.repository_root()
        self.policy = release_lib.load_policy()
        self.assets = create_source_assets(self.root, self.policy)

    def close(self) -> None:
        self.temporary.cleanup()

    def stage(
        self,
        platform: str,
        *,
        name: str | None = None,
        identity: dict[str, Any] | None = None,
        assets: list[str] | None = None,
    ) -> Path:
        binary, redistributable = create_binary_inputs(
            self.root, platform, self.policy
        )
        output = self.root / "stages" / (name or platform)
        release_lib.stage_candidate(
            source_root=self.root,
            policy=self.policy,
            platform=platform,
            binary=binary,
            redistributable=redistributable,
            output=output,
            release_label=TEST_RELEASE_LABEL,
            app_id=TEST_APP_ID,
            source_commit=TEST_COMMIT,
            runtime_assets=assets or self.assets,
            macos_bundle_id=(
                "club.animalfighter.game" if platform == "macos-universal" else None
            ),
            macos_bundle_version=("7" if platform == "macos-universal" else None),
            macos_min_version=("12.0" if platform == "macos-universal" else None),
            identity_provider=lambda _binary, _platform, _root: (
                identity or fixture_identity()
            ),
        )
        return output


class ReleasePolicyTests(unittest.TestCase):
    def test_policy_is_exact_and_pins_primary_runtime_identity(self) -> None:
        policy = release_lib.load_policy()
        runtime = policy["steam_linux_runtime"]
        self.assertEqual(runtime["app_id"], 4_183_110)
        self.assertEqual(runtime["sdk_tag"], "4.0.20260714.251823")
        self.assertEqual(
            runtime["sdk_digest"],
            "sha256:2c4c6520a268ef53255d511ae5988e35855b39a4b6c1e9865d56e5011c76ec3e",
        )

    def test_unknown_policy_fields_fail_closed(self) -> None:
        policy = release_lib.load_policy()
        policy["unexpected"] = True
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "policy.json"
            path.write_text(json.dumps(policy), encoding="utf-8")
            with self.assertRaises(release_lib.ReleaseError):
                release_lib.load_policy(path)

    def test_release_labels_ids_and_macos_values_are_strict(self) -> None:
        valid = release_lib.validate_release_inputs(
            TEST_RELEASE_LABEL,
            TEST_APP_ID,
            TEST_DEPOT_IDS,
            macos_bundle_id="club.animalfighter.game",
            macos_bundle_version="7.2",
            macos_min_version="12.0",
        )
        self.assertEqual(valid["app_id"], TEST_APP_ID)
        for label in ("", ".", "..", "-rc", "has space", "x" * 65):
            with self.subTest(label=label):
                with self.assertRaises(release_lib.ReleaseError):
                    release_lib.validate_release_label(label)
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.validate_release_inputs(
                TEST_RELEASE_LABEL, 480, TEST_DEPOT_IDS
            )
        duplicate = dict(TEST_DEPOT_IDS)
        duplicate["linux-x86_64"] = duplicate["windows-x86_64"]
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.validate_release_inputs(
                TEST_RELEASE_LABEL, TEST_APP_ID, duplicate
            )

    def test_portable_paths_and_casefold_collisions_fail_closed(self) -> None:
        for path in (
            "../escape",
            "/absolute",
            "a\\b",
            "C:/drive",
            "trailing.",
            "assets/CON.txt",
            "assets/com1.model",
        ):
            with self.subTest(path=path):
                with self.assertRaises(release_lib.ReleaseError):
                    release_lib.portable_relative_path(path)
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.ensure_casefold_unique(
                ["assets/Fighter.glb", "assets/fighter.glb"], "fixture"
            )

    def test_identity_environment_removes_loader_and_build_overrides(self) -> None:
        with mock.patch.dict(
            os.environ,
            {
                "AFC_STEAM_APP_ID": "480",
                "DYLD_INSERT_LIBRARIES": "/tmp/injected.dylib",
                "LD_PRELOAD": "/tmp/injected.so",
                "LD_LIBRARY_PATH": "/tmp/old",
                "KEEP_ME": "yes",
            },
            clear=True,
        ):
            environment = release_lib.sanitized_identity_environment(
                "linux-x86_64", Path("/candidate")
            )
        self.assertEqual(environment["KEEP_ME"], "yes")
        self.assertEqual(environment["LD_LIBRARY_PATH"], "/candidate")
        self.assertNotIn("AFC_STEAM_APP_ID", environment)
        self.assertNotIn("DYLD_INSERT_LIBRARIES", environment)
        self.assertNotIn("LD_PRELOAD", environment)


class StageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_all_platform_layouts_seal_and_verify(self) -> None:
        for platform in self.fixture.policy["platforms"]:
            with self.subTest(platform=platform):
                stage = self.fixture.stage(platform)
                manifest = release_lib.verify_stage(
                    stage,
                    self.fixture.policy,
                    expected_platform=platform,
                    expected_release_label=TEST_RELEASE_LABEL,
                    expected_app_id=TEST_APP_ID,
                    expected_source_commit=TEST_COMMIT,
                )
                self.assertEqual(manifest["release_identity"], fixture_identity())
                payload = {record["path"] for record in manifest["payload"]}
                self.assertNotIn("steam_appid.txt", {Path(path).name for path in payload})
                self.assertFalse(
                    any(path.endswith("character_move_sets.ron") for path in payload)
                )
                if platform == "linux-x86_64":
                    launcher = (stage / "afc-launch").read_text(encoding="ascii")
                    self.assertIn("LD_LIBRARY_PATH", launcher)
                    self.assertEqual(
                        manifest["steam"]["linux_runtime"],
                        self.fixture.policy["steam_linux_runtime"],
                    )
                elif platform == "macos-universal":
                    self.assertTrue(
                        (
                            stage
                            / "Animal Fighter Club.app"
                            / "Contents"
                            / "Info.plist"
                        ).is_file()
                    )

    def test_payload_tampering_missing_and_extra_files_are_rejected(self) -> None:
        stage = self.fixture.stage("linux-x86_64")
        asset = stage / "assets" / "characters" / "kenney_cube_pets" / "animal-cat.glb"
        asset.write_bytes(b"tampered")
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.verify_stage(stage, self.fixture.policy)

        stage = self.fixture.stage("windows-x86_64", name="missing")
        (stage / "assets" / "steam_input" / "action_manifest.vdf").unlink()
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.verify_stage(stage, self.fixture.policy)

        stage = self.fixture.stage("windows-x86_64", name="extra")
        (stage / "unexpected.txt").write_text("extra", encoding="utf-8")
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.verify_stage(stage, self.fixture.policy)

    def test_identity_and_embedded_asset_leaks_fail_before_publish(self) -> None:
        with self.assertRaises(release_lib.ReleaseError):
            self.fixture.stage(
                "windows-x86_64",
                name="nonshipping",
                identity=fixture_identity(shipping=False),
            )
        self.assertFalse((self.fixture.root / "stages" / "nonshipping").exists())

        leaked_assets = [
            *self.fixture.assets,
            "assets/characters/character_move_sets.ron",
        ]
        with self.assertRaises(release_lib.ReleaseError):
            self.fixture.stage(
                "linux-x86_64", name="embedded-leak", assets=leaked_assets
            )
        self.assertFalse((self.fixture.root / "stages" / "embedded-leak").exists())

        with self.assertRaises(release_lib.ReleaseError):
            self.fixture.stage(
                "macos-universal",
                name="invalid-macos-version",
                identity=fixture_identity(product_version="0.1.0-rc.1"),
            )
        self.assertFalse(
            (self.fixture.root / "stages" / "invalid-macos-version").exists()
        )

    def test_unknown_manifest_fields_and_symlinks_fail_closed(self) -> None:
        stage = self.fixture.stage("linux-x86_64")
        manifest_path = stage / "release-manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["unknown"] = True
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.verify_stage(stage, self.fixture.policy)

        if hasattr(os, "symlink"):
            stage = self.fixture.stage("linux-x86_64", name="symlink")
            try:
                os.symlink("ffc-prototype", stage / "alias")
            except OSError:
                self.skipTest("host does not permit symlink creation")
            with self.assertRaises(release_lib.ReleaseError):
                release_lib.verify_stage(stage, self.fixture.policy)

        if hasattr(os, "symlink"):
            stage = self.fixture.stage("windows-x86_64", name="stage-root-target")
            stage_link = self.fixture.root / "stages" / "stage-root-link"
            try:
                os.symlink(stage, stage_link, target_is_directory=True)
            except OSError:
                self.skipTest("host does not permit directory symlink creation")
            with self.assertRaisesRegex(release_lib.ReleaseError, "must not be a symlink"):
                release_lib.verify_stage(stage_link, self.fixture.policy)

    def test_manifest_requires_structural_paths_and_executable_semantics(self) -> None:
        stage = self.fixture.stage("linux-x86_64")
        manifest_path = stage / "release-manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        binary_record = next(
            record
            for record in manifest["payload"]
            if record["path"] == "ffc-prototype"
        )
        binary_record["executable"] = False
        manifest_path.write_bytes(release_lib.deterministic_json_bytes(manifest))
        with self.assertRaisesRegex(
            release_lib.ReleaseError, "executable flag differs"
        ):
            release_lib.verify_stage(stage, self.fixture.policy)

        stage = self.fixture.stage("linux-x86_64", name="missing-structural")
        manifest_path = stage / "release-manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["payload"] = [
            record
            for record in manifest["payload"]
            if record["path"] != "ffc-prototype"
        ]
        manifest_path.write_bytes(release_lib.deterministic_json_bytes(manifest))
        with self.assertRaisesRegex(
            release_lib.ReleaseError, "missing required structural"
        ):
            release_lib.verify_stage(stage, self.fixture.policy)

    def test_binary_format_and_redistributable_discovery_are_exact(self) -> None:
        invalid = self.fixture.root / "invalid"
        invalid.write_bytes(b"not a binary")
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.validate_binary_format(invalid, "elf-x86_64")

        search = self.fixture.root / "target" / "release" / "build"
        first = search / "steamworks-sys-abc" / "out" / "libsteam_api.so"
        write_elf_x86_64(first)
        found = release_lib.find_redistributable(
            search, self.fixture.policy, "linux-x86_64"
        )
        self.assertEqual(found, first.resolve())
        write_elf_x86_64(
            search / "steamworks-sys-def" / "out" / "libsteam_api.so"
        )
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.find_redistributable(
                search, self.fixture.policy, "linux-x86_64"
            )


class ArchiveAndVdfTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_archives_are_byte_deterministic_and_rooted_at_depot_root(self) -> None:
        stage = self.fixture.stage("windows-x86_64")
        first = self.fixture.root / "archives" / "first.zip"
        second = self.fixture.root / "archives" / "second.zip"
        one = release_lib.deterministic_archive(
            stage, first, self.fixture.policy
        )
        two = release_lib.deterministic_archive(
            stage, second, self.fixture.policy
        )
        self.assertEqual(one["sha256"], two["sha256"])
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            names = archive.namelist()
        self.assertIn("ffc-prototype.exe", names)
        self.assertFalse(any(name.startswith("windows-x86_64/") for name in names))

    def test_identity_comparison_and_preview_vdf_rendering(self) -> None:
        stages = {
            platform: self.fixture.stage(platform)
            for platform in self.fixture.policy["platforms"]
        }
        comparison = release_lib.compare_identities(
            list(stages.values()), self.fixture.policy
        )
        self.assertEqual(
            set(comparison["platforms"]), set(self.fixture.policy["platforms"])
        )
        vdf_output = self.fixture.root / "steam-vdf"
        result = release_lib.render_steam_vdfs(
            root=self.fixture.repository_root,
            policy=self.fixture.policy,
            stages=stages,
            app_id=TEST_APP_ID,
            depot_ids=TEST_DEPOT_IDS,
            release_label=TEST_RELEASE_LABEL,
            source_commit=TEST_COMMIT,
            build_output=self.fixture.root / "steam-build-output",
            output=vdf_output,
            enforce_output_location=False,
        )
        app_vdf = Path(result["app_build"]).read_text(encoding="utf-8")
        self.assertIn('"Preview" "1"', app_vdf)
        self.assertNotIn("SetLive", app_vdf)
        self.assertNotIn("steamcmd", app_vdf.casefold())
        self.assertEqual(len(list(vdf_output.glob("depot_build_*.vdf"))), 3)
        with self.assertRaisesRegex(
            release_lib.ReleaseError, "exactly one candidate"
        ):
            release_lib.compare_identities(
                list(stages.values())[:2], self.fixture.policy
            )

    def test_vdf_injection_upload_tokens_and_unresolved_fields_are_rejected(self) -> None:
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.validate_preview_vdf(
                '"AppBuild"\n{\n"Preview" "0"\n"SetLive" "default"\n}\n'
            )
        with self.assertRaisesRegex(release_lib.ReleaseError, "exactly one Preview"):
            release_lib.validate_preview_vdf(
                '"AppBuild"\n{\n'
                '    "Preview" "1"\n'
                '    "Preview" "0"\n'
                '}\n'
            )
        release_lib.validate_preview_vdf(
            '"DepotBuildConfig"\n{\n'
            '    "ContentRoot" "/home/username/candidate"\n'
            '}\n'
        )
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.validate_preview_vdf(
                '"AppBuild"\n{\n'
                '    "Preview" "1"\n'
                '    "Username" "release-operator"\n'
                '}\n'
            )
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.render_template(
                self.fixture.repository_root
                / "packaging"
                / "steam"
                / "depot_build.vdf.in",
                {"DEPOT_ID": "123"},
            )
        with self.assertRaises(release_lib.ReleaseError):
            release_lib.vdf_escape('bad\n"value')


class GitCleanlinessTests(unittest.TestCase):
    def test_clean_modified_and_untracked_states_are_distinguished(self) -> None:
        if shutil.which("git") is None:
            self.skipTest("git is not installed")
        with tempfile.TemporaryDirectory(prefix="afc-release-git-test-") as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(
                ["git", "config", "user.email", "release-test@example.invalid"],
                cwd=root,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "AFC Release Test"],
                cwd=root,
                check=True,
            )
            (root / ".gitignore").write_text("target/\n", encoding="utf-8")
            tracked = root / "tracked.txt"
            tracked.write_text("original\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(
                ["git", "commit", "-q", "-m", "fixture"], cwd=root, check=True
            )
            commit = release_lib.ensure_clean_git_tree(root)
            self.assertRegex(commit, r"^[0-9a-f]{40}$")

            ignored = root / "target" / "ignored"
            ignored.parent.mkdir()
            ignored.write_text("ignored", encoding="utf-8")
            self.assertEqual(release_lib.ensure_clean_git_tree(root), commit)

            tracked.write_text("modified\n", encoding="utf-8")
            with self.assertRaises(release_lib.ReleaseError):
                release_lib.ensure_clean_git_tree(root)
            tracked.write_text("original\n", encoding="utf-8")
            (root / "untracked.txt").write_text("untracked\n", encoding="utf-8")
            with self.assertRaises(release_lib.ReleaseError):
                release_lib.ensure_clean_git_tree(root)


def run(*, verbosity: int = 1) -> bool:
    suite = unittest.defaultTestLoader.loadTestsFromModule(
        __import__(__name__)
    )
    result = unittest.TextTestRunner(verbosity=max(1, verbosity)).run(suite)
    return result.wasSuccessful()


if __name__ == "__main__":
    raise SystemExit(0 if run(verbosity=2) else 1)
