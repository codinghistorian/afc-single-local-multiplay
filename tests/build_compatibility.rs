#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

use std::path::Path;

use build_script::{
    GAMEPLAY_SOURCES, NamedInput, ReleaseBuildInputs, build_metadata_inputs, build_source_inputs,
    canonical_text_bytes, collect_rust_source_paths, expanded_digest, gameplay_source_inputs,
    parse_steam_app_id, validate_build_label, validate_release_build,
};

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn mutate_named_input(inputs: &[NamedInput], name: &str) -> Vec<NamedInput> {
    let mut changed = inputs.to_vec();
    changed
        .iter_mut()
        .find(|input| input.name == name)
        .unwrap_or_else(|| panic!("missing compatibility input {name}"))
        .bytes
        .push(0xA5);
    changed
}

#[test]
fn rust_source_discovery_is_recursive_sorted_and_covers_shipping_boundaries() {
    let paths = collect_rust_source_paths(project_root()).unwrap();
    assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));

    for required in [
        "src/lib.rs",
        "src/main.rs",
        "src/native_online_app.rs",
        "src/user_mode.rs",
        "src/camera.rs",
        "src/multiplayer_diagnostics.rs",
        "src/snapshot_ecs.rs",
        "src/bin/afc-dedicated.rs",
        "src/bin/afc-multiplayer-profile.rs",
    ] {
        assert!(
            paths.iter().any(|path| path == required),
            "{required} escaped build identity"
        );
    }
}

#[test]
fn build_digest_is_path_stable_and_sensitive_to_each_critical_boundary() {
    let inputs = build_source_inputs(project_root()).unwrap();
    let baseline = expanded_digest("afc-build-v2", &inputs, 16);

    let mut reordered = inputs.clone();
    reordered.reverse();
    assert_eq!(
        expanded_digest("afc-build-v2", &reordered, 16),
        baseline,
        "filesystem enumeration order must not affect identity"
    );

    for required in [
        "rust-toolchain.toml",
        "src/lib.rs",
        "src/main.rs",
        "src/native_online_app.rs",
        "src/user_mode.rs",
        "src/camera.rs",
        "src/multiplayer_diagnostics.rs",
        "src/snapshot_ecs.rs",
    ] {
        let changed = mutate_named_input(&inputs, required);
        assert_ne!(
            expanded_digest("afc-build-v2", &changed, 16),
            baseline,
            "{required} did not affect build identity"
        );
    }

    let renamed = vec![NamedInput::new("src/renamed.rs", b"same bytes".to_vec())];
    let original = vec![NamedInput::new("src/original.rs", b"same bytes".to_vec())];
    assert_ne!(
        expanded_digest("afc-build-v2", &renamed, 16),
        expanded_digest("afc-build-v2", &original, 16),
        "relative paths are part of build identity"
    );
}

#[test]
fn every_release_metadata_boundary_changes_build_identity() {
    fn metadata_digest(
        package: &str,
        version: &str,
        profile: &str,
        label: &str,
        app_id: Option<u32>,
        features: &[String],
    ) -> Vec<u8> {
        expanded_digest(
            "afc-build-v2",
            &build_metadata_inputs(package, version, profile, label, app_id, features),
            16,
        )
    }

    let baseline_features = vec![
        "NATIVE".to_owned(),
        "SHIPPING".to_owned(),
        "STEAM_NET".to_owned(),
    ];
    let baseline = metadata_digest(
        "ffc-prototype",
        "0.1.0",
        "release",
        "candidate.1",
        Some(123_456),
        &baseline_features,
    );

    for changed in [
        metadata_digest(
            "renamed-package",
            "0.1.0",
            "release",
            "candidate.1",
            Some(123_456),
            &baseline_features,
        ),
        metadata_digest(
            "ffc-prototype",
            "0.1.1",
            "release",
            "candidate.1",
            Some(123_456),
            &baseline_features,
        ),
        metadata_digest(
            "ffc-prototype",
            "0.1.0",
            "debug",
            "candidate.1",
            Some(123_456),
            &baseline_features,
        ),
        metadata_digest(
            "ffc-prototype",
            "0.1.0",
            "release",
            "candidate.2",
            Some(123_456),
            &baseline_features,
        ),
        metadata_digest(
            "ffc-prototype",
            "0.1.0",
            "release",
            "candidate.1",
            Some(654_321),
            &baseline_features,
        ),
        metadata_digest(
            "ffc-prototype",
            "0.1.0",
            "release",
            "candidate.1",
            Some(123_456),
            &["NATIVE".to_owned(), "STEAM_NET".to_owned()],
        ),
    ] {
        assert_ne!(changed, baseline);
    }

    let mut reordered_features = baseline_features;
    reordered_features.reverse();
    assert_eq!(
        metadata_digest(
            "ffc-prototype",
            "0.1.0",
            "release",
            "candidate.1",
            Some(123_456),
            &reordered_features,
        ),
        baseline,
        "Cargo feature enumeration order must not affect build identity"
    );
}

fn release_inputs() -> ReleaseBuildInputs<'static> {
    ReleaseBuildInputs {
        profile: "release",
        release_label: "steam-rc.1+abc123",
        steam_app_id: Some(123_456),
        shipping_enabled: true,
        steam_net_enabled: true,
        native_enabled: true,
        dev_hot_reload_enabled: false,
    }
}

#[test]
fn release_label_validation_is_strict_and_directive_safe() {
    for valid in ["development", "rc-1", "steam_1.2+abc", "A1"] {
        validate_build_label(valid).unwrap();
    }
    for invalid in [
        "",
        ".",
        "..",
        "-rc1",
        "_rc1",
        "+rc1",
        "contains space",
        "path/to/build",
        "line\nbreak",
        "unicode-한글",
    ] {
        assert!(validate_build_label(invalid).is_err(), "{invalid:?}");
    }
    assert!(validate_build_label(&"a".repeat(64)).is_ok());
    assert!(validate_build_label(&"a".repeat(65)).is_err());
}

#[test]
fn shipping_release_validation_is_fail_closed() {
    validate_release_build(release_inputs()).unwrap();

    let mut invalid = release_inputs();
    invalid.release_label = "development";
    assert!(validate_release_build(invalid).is_err());

    let mut invalid = release_inputs();
    invalid.steam_app_id = None;
    assert!(validate_release_build(invalid).is_err());

    let mut invalid = release_inputs();
    invalid.steam_app_id = Some(0);
    assert!(validate_release_build(invalid).is_err());

    let mut invalid = release_inputs();
    invalid.steam_app_id = Some(480);
    assert!(validate_release_build(invalid).is_err());

    let mut invalid = release_inputs();
    invalid.native_enabled = false;
    assert!(validate_release_build(invalid).is_err());

    let mut invalid = release_inputs();
    invalid.steam_net_enabled = false;
    assert!(validate_release_build(invalid).is_err());

    let mut invalid = release_inputs();
    invalid.dev_hot_reload_enabled = true;
    assert!(validate_release_build(invalid).is_err());
}

#[test]
fn debug_feature_union_is_not_mislabeled_as_a_release_artifact() {
    let mut debug = release_inputs();
    debug.profile = "debug";
    debug.release_label = "development";
    debug.steam_app_id = None;
    debug.dev_hot_reload_enabled = true;
    validate_release_build(debug).unwrap();
}

#[test]
fn steam_app_id_parser_accepts_only_nonzero_decimal_u32() {
    assert_eq!(parse_steam_app_id("123456").unwrap(), 123_456);
    for invalid in ["", "0", "-1", "+1", " 1", "1.0", "4294967296"] {
        assert!(parse_steam_app_id(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn checked_out_line_endings_have_one_cross_platform_digest_representation() {
    assert_eq!(
        canonical_text_bytes(b"first\r\nsecond\r\n"),
        canonical_text_bytes(b"first\nsecond\n")
    );
}

#[test]
fn gameplay_digest_includes_frozen_authorship_but_excludes_presentation_and_tests() {
    let inputs = gameplay_source_inputs(project_root()).unwrap();
    let baseline = expanded_digest("afc-gameplay-content-v2", &inputs, 32);

    for required in [
        "src/arena.rs",
        "src/canonical_math.rs",
        "src/live_input.rs",
        "arts/champions_court.ron",
        "assets/characters/character_move_sets.ron",
        "assets/feel/combat_overrides.ron",
    ] {
        assert!(
            GAMEPLAY_SOURCES.contains(&required),
            "{required} is not classified as gameplay content"
        );
        let changed = mutate_named_input(&inputs, required);
        assert_ne!(
            expanded_digest("afc-gameplay-content-v2", &changed, 32),
            baseline,
            "{required} did not affect gameplay identity"
        );
    }

    for excluded in [
        "src/lib.rs",
        "src/main.rs",
        "src/camera.rs",
        "src/combat_sfx.rs",
        "src/effects.rs",
        "src/hud.rs",
        "src/native_online_app.rs",
        "src/network_lab_tests.rs",
        "src/simulation_harness.rs",
    ] {
        assert!(
            !GAMEPLAY_SOURCES.contains(&excluded),
            "{excluded} must not affect gameplay-content identity"
        );
    }
}
