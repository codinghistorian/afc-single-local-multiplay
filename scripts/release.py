#!/usr/bin/env python3
"""Animal Fighter Club release packaging command-line interface."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Sequence

import release_lib


def _print_json(value: Any) -> None:
    print(json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True))


def _root_and_policy(arguments: argparse.Namespace) -> tuple[Path, dict[str, Any]]:
    root = Path(arguments.root).resolve()
    policy_path = (
        Path(arguments.policy).resolve()
        if arguments.policy
        else release_lib.default_policy_path(root)
    )
    return root, release_lib.load_policy(policy_path)


def _depot_ids(arguments: argparse.Namespace) -> dict[str, str]:
    return {
        "windows-x86_64": arguments.windows_depot_id,
        "linux-x86_64": arguments.linux_depot_id,
        "macos-universal": arguments.macos_depot_id,
    }


def command_self_test(arguments: argparse.Namespace) -> None:
    import release_self_test

    success = release_self_test.run(verbosity=arguments.verbosity)
    if not success:
        raise release_lib.ReleaseError("release-tool self-tests failed")


def command_audit_source(arguments: argparse.Namespace) -> None:
    root, policy = _root_and_policy(arguments)
    result = release_lib.audit_source(root, policy, require_clean=True)
    _print_json(
        {
            "source_commit": result["source_commit"],
            "tracked_file_count": result["tracked_file_count"],
            "runtime_asset_count": len(result["runtime_assets"]),
            "clean": True,
        }
    )


def command_validate_inputs(arguments: argparse.Namespace) -> None:
    _, policy = _root_and_policy(arguments)
    values = release_lib.validate_release_inputs(
        arguments.release_label,
        arguments.app_id,
        _depot_ids(arguments),
        macos_bundle_id=arguments.macos_bundle_id,
        macos_bundle_version=arguments.macos_bundle_version,
        macos_min_version=arguments.macos_min_version,
    )
    runtime = policy["steam_linux_runtime"]
    _print_json(
        {
            **values,
            "steam_linux_runtime": {
                "name": runtime["name"],
                "app_id": runtime["app_id"],
                "sdk_reference": (
                    f"{runtime['sdk_image']}:{runtime['sdk_tag']}"
                    f"@{runtime['sdk_digest']}"
                ),
            },
        }
    )


def command_find_redistributable(arguments: argparse.Namespace) -> None:
    _, policy = _root_and_policy(arguments)
    path = release_lib.find_redistributable(
        Path(arguments.search_root).resolve(), policy, arguments.platform
    )
    print(path)


def command_stage(arguments: argparse.Namespace) -> None:
    root, policy = _root_and_policy(arguments)
    if arguments.redistributable:
        redistributable = Path(arguments.redistributable).resolve()
    else:
        redistributable = release_lib.find_redistributable(
            Path(arguments.redistributable_search_root).resolve(),
            policy,
            arguments.platform,
        )
    app_id = release_lib.validate_u32_id(arguments.app_id, "Steam App ID")
    manifest = release_lib.stage_from_repository(
        root=root,
        policy=policy,
        platform=arguments.platform,
        binary=Path(arguments.binary),
        redistributable=redistributable,
        output=Path(arguments.output),
        release_label=arguments.release_label,
        app_id=app_id,
        macos_bundle_id=arguments.macos_bundle_id,
        macos_bundle_version=arguments.macos_bundle_version,
        macos_min_version=arguments.macos_min_version,
    )
    _print_json(
        {
            "output": str(Path(arguments.output).resolve()),
            "platform": manifest["platform"],
            "source_commit": manifest["source_commit"],
            "release_identity": manifest["release_identity"],
            "payload_files": len(manifest["payload"]),
        }
    )


def command_verify(arguments: argparse.Namespace) -> None:
    _, policy = _root_and_policy(arguments)
    app_id = (
        release_lib.validate_u32_id(arguments.app_id, "expected Steam App ID")
        if arguments.app_id is not None
        else None
    )
    manifest = release_lib.verify_stage(
        Path(arguments.stage),
        policy,
        expected_platform=arguments.platform,
        expected_release_label=arguments.release_label,
        expected_app_id=app_id,
        expected_source_commit=arguments.source_commit,
    )
    _print_json(
        {
            "verified": True,
            "platform": manifest["platform"],
            "source_commit": manifest["source_commit"],
            "release_identity": manifest["release_identity"],
            "payload_files": len(manifest["payload"]),
        }
    )


def command_archive(arguments: argparse.Namespace) -> None:
    root, policy = _root_and_policy(arguments)
    output = release_lib.ensure_output_below(root, Path(arguments.output), "dist")
    result = release_lib.deterministic_archive(
        Path(arguments.stage), output, policy
    )
    _print_json(result)


def command_compare_identities(arguments: argparse.Namespace) -> None:
    _, policy = _root_and_policy(arguments)
    result = release_lib.compare_identities(
        [Path(path) for path in arguments.candidates], policy
    )
    _print_json(result)


def command_render_steam_vdf(arguments: argparse.Namespace) -> None:
    root, policy = _root_and_policy(arguments)
    result = release_lib.render_steam_vdfs(
        root=root,
        policy=policy,
        stages={
            "windows-x86_64": Path(arguments.windows_stage),
            "linux-x86_64": Path(arguments.linux_stage),
            "macos-universal": Path(arguments.macos_stage),
        },
        app_id=release_lib.validate_u32_id(arguments.app_id, "Steam App ID"),
        depot_ids={
            platform: release_lib.validate_u32_id(depot_id, f"{platform} depot ID")
            for platform, depot_id in _depot_ids(arguments).items()
        },
        release_label=arguments.release_label,
        source_commit=arguments.source_commit,
        build_output=Path(arguments.build_output),
        output=Path(arguments.output),
    )
    _print_json(result)


def _add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--root",
        default=str(release_lib.repository_root()),
        help="repository root (default: inferred from this script)",
    )
    parser.add_argument(
        "--policy",
        help="release-policy JSON path (default: <root>/packaging/release-policy.json)",
    )


def _add_depot_ids(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--windows-depot-id", required=True)
    parser.add_argument("--linux-depot-id", required=True)
    parser.add_argument("--macos-depot-id", required=True)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Audit, stage, verify, and archive AFC candidates. This tool never "
            "builds code or uploads to Steam."
        )
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    self_test = subparsers.add_parser(
        "self-test", help="run standard-library synthetic release-tool tests"
    )
    self_test.add_argument("-v", "--verbosity", action="count", default=1)
    self_test.set_defaults(handler=command_self_test)

    audit = subparsers.add_parser(
        "audit-source", help="require a clean, complete, tracked release source tree"
    )
    _add_common(audit)
    audit.set_defaults(handler=command_audit_source)

    validate = subparsers.add_parser(
        "validate-inputs", help="validate release IDs and immutable product inputs"
    )
    _add_common(validate)
    validate.add_argument("--release-label", required=True)
    validate.add_argument("--app-id", required=True)
    _add_depot_ids(validate)
    validate.add_argument("--macos-bundle-id", required=True)
    validate.add_argument("--macos-bundle-version", required=True)
    validate.add_argument("--macos-min-version", required=True)
    validate.set_defaults(handler=command_validate_inputs)

    find = subparsers.add_parser(
        "find-redistributable",
        help="find the one Steam API library copied by steamworks-sys",
    )
    _add_common(find)
    find.add_argument(
        "--platform",
        choices=("windows-x86_64", "linux-x86_64", "macos-universal"),
        required=True,
    )
    find.add_argument("--search-root", required=True)
    find.set_defaults(handler=command_find_redistributable)

    stage = subparsers.add_parser(
        "stage", help="assemble and atomically seal a clean-tree candidate"
    )
    _add_common(stage)
    stage.add_argument(
        "--platform",
        choices=("windows-x86_64", "linux-x86_64", "macos-universal"),
        required=True,
    )
    stage.add_argument("--binary", required=True)
    redistributable = stage.add_mutually_exclusive_group(required=True)
    redistributable.add_argument("--redistributable")
    redistributable.add_argument("--redistributable-search-root")
    stage.add_argument("--output", required=True)
    stage.add_argument("--release-label", required=True)
    stage.add_argument("--app-id", required=True)
    stage.add_argument("--macos-bundle-id")
    stage.add_argument("--macos-bundle-version")
    stage.add_argument("--macos-min-version")
    stage.set_defaults(handler=command_stage)

    verify = subparsers.add_parser(
        "verify", help="verify a sealed candidate without Git, Cargo, or Steam"
    )
    _add_common(verify)
    verify.add_argument("--stage", required=True)
    verify.add_argument(
        "--platform",
        choices=("windows-x86_64", "linux-x86_64", "macos-universal"),
    )
    verify.add_argument("--release-label")
    verify.add_argument("--app-id")
    verify.add_argument("--source-commit")
    verify.set_defaults(handler=command_verify)

    archive = subparsers.add_parser(
        "archive", help="create a deterministic stored ZIP and SHA-256 sidecar"
    )
    _add_common(archive)
    archive.add_argument("--stage", required=True)
    archive.add_argument("--output", required=True)
    archive.set_defaults(handler=command_archive)

    compare = subparsers.add_parser(
        "compare-identities",
        help="require exact cross-platform identity and source-commit parity",
    )
    _add_common(compare)
    compare.add_argument("candidates", nargs="+")
    compare.set_defaults(handler=command_compare_identities)

    render = subparsers.add_parser(
        "render-steam-vdf",
        help="render balanced preview-only SteamPipe VDFs; never upload",
    )
    _add_common(render)
    render.add_argument("--release-label", required=True)
    render.add_argument("--source-commit", required=True)
    render.add_argument("--app-id", required=True)
    _add_depot_ids(render)
    render.add_argument("--windows-stage", required=True)
    render.add_argument("--linux-stage", required=True)
    render.add_argument("--macos-stage", required=True)
    render.add_argument("--build-output", required=True)
    render.add_argument("--output", required=True)
    render.set_defaults(handler=command_render_steam_vdf)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv)
    try:
        arguments.handler(arguments)
        return 0
    except release_lib.ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    except BrokenPipeError:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
