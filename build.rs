use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01b3;
const BUILD_LABEL_ENV: &str = "AFC_BUILD_ID";
const STEAM_APP_ID_ENV: &str = "AFC_STEAM_APP_ID";
const DEVELOPMENT_BUILD_LABEL: &str = "development";
const MAX_BUILD_LABEL_BYTES: usize = 64;
const SPACEWAR_APP_ID: u32 = 480;

const ROOT_BUILD_SOURCES: &[&str] = &[
    "build.rs",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

pub(crate) const GAMEPLAY_SOURCES: &[&str] = &[
    "src/arena.rs",
    "src/arena_barriers.rs",
    "src/arena_defs.rs",
    "src/arena_prop_colliders.rs",
    "src/bee_skills.rs",
    "src/body_collision.rs",
    "src/bot.rs",
    "src/canonical_math.rs",
    "src/canonical_state.rs",
    "src/characters.rs",
    "src/chick_skills.rs",
    "src/combat.rs",
    "src/components.rs",
    "src/constants.rs",
    "src/contact_arbitration.rs",
    "src/equipment.rs",
    "src/feel.rs",
    "src/fighter.rs",
    "src/game_state.rs",
    "src/items.rs",
    "src/live_input.rs",
    "src/penguin_skills.rs",
    "src/reactions.rs",
    "src/sim_event.rs",
    "src/simulation.rs",
    "src/specials.rs",
    "src/styles.rs",
    "src/techniques.rs",
    "src/tick_input.rs",
    "arts/champions_court.ron",
    "assets/camera/single_player_camera.ron",
    "assets/characters/character_move_sets.ron",
    "assets/feel/combat_overrides.ron",
    "assets/maps/overlays/arena_0.ron",
    "assets/maps/overlays/arena_1.ron",
    "assets/maps/overlays/arena_2.ron",
    "assets/maps/overlays/arena_3.ron",
    "assets/maps/overlays/arena_4.ron",
    "assets/maps/overlays/arena_5.ron",
    "assets/maps/overlays/arena_6.ron",
    "assets/maps/overlays/arena_7.ron",
    "assets/maps/overlays/arena_8.ron",
    "assets/maps/overlays/arena_9.ron",
];

fn main() {
    let project_root =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo supplies CARGO_MANIFEST_DIR"));
    let rust_sources = collect_rust_source_paths(&project_root)
        .unwrap_or_else(|error| panic!("failed to classify Rust compatibility inputs: {error}"));

    println!("cargo:rerun-if-env-changed={BUILD_LABEL_ENV}");
    println!("cargo:rerun-if-env-changed={STEAM_APP_ID_ENV}");
    // Watching the directory makes additions and removals rerun this script;
    // watching every classified file makes ordinary edits explicit as well.
    println!("cargo:rerun-if-changed=src");
    for path in ROOT_BUILD_SOURCES
        .iter()
        .copied()
        .chain(rust_sources.iter().map(String::as_str))
        .chain(GAMEPLAY_SOURCES.iter().copied())
    {
        println!("cargo:rerun-if-changed={path}");
    }

    let package = env::var("CARGO_PKG_NAME").expect("Cargo supplies CARGO_PKG_NAME");
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo supplies CARGO_PKG_VERSION");
    let profile = env::var("PROFILE").expect("Cargo supplies PROFILE");
    let configured =
        env::var(BUILD_LABEL_ENV).unwrap_or_else(|_| DEVELOPMENT_BUILD_LABEL.to_owned());
    let steam_app_id = env::var(STEAM_APP_ID_ENV)
        .ok()
        .map(|raw| parse_steam_app_id(&raw))
        .transpose()
        .unwrap_or_else(|error| panic!("{error}"));
    let native_enabled = env::var_os("CARGO_FEATURE_NATIVE").is_some();
    let dev_hot_reload_enabled = env::var_os("CARGO_FEATURE_DEV_HOT_RELOAD").is_some();
    let steam_net_enabled = env::var_os("CARGO_FEATURE_STEAM_NET").is_some();
    let shipping_enabled = env::var_os("CARGO_FEATURE_SHIPPING").is_some();
    let release_shipping = profile == "release" && shipping_enabled;
    validate_release_build(ReleaseBuildInputs {
        profile: &profile,
        release_label: &configured,
        steam_app_id,
        shipping_enabled,
        steam_net_enabled,
        native_enabled,
        dev_hot_reload_enabled,
    })
    .unwrap_or_else(|error| panic!("{error}"));

    println!("cargo:rustc-env=AFC_COMPILED_RELEASE_LABEL={configured}");
    println!(
        "cargo:rustc-env=AFC_COMPILED_SHIPPING={}",
        u8::from(release_shipping)
    );
    if let Some(app_id) = steam_app_id {
        println!("cargo:rustc-env=AFC_COMPILED_STEAM_APP_ID={app_id}");
    }

    let enabled_features = enabled_cargo_features();
    let mut build_inputs = build_source_inputs(&project_root)
        .unwrap_or_else(|error| panic!("failed to read build compatibility inputs: {error}"));
    build_inputs.extend(build_metadata_inputs(
        &package,
        &version,
        &profile,
        &configured,
        steam_app_id,
        &enabled_features,
    ));
    let gameplay_inputs = gameplay_source_inputs(&project_root)
        .unwrap_or_else(|error| panic!("failed to read gameplay compatibility inputs: {error}"));

    println!(
        "cargo:rustc-env=AFC_COMPILED_BUILD_ID={}",
        hex(&expanded_digest("afc-build-v2", &build_inputs, 16))
    );
    println!(
        "cargo:rustc-env=AFC_COMPILED_GAMEPLAY_CONTENT_HASH={}",
        hex(&expanded_digest(
            "afc-gameplay-content-v2",
            &gameplay_inputs,
            32
        ))
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseBuildInputs<'a> {
    pub(crate) profile: &'a str,
    pub(crate) release_label: &'a str,
    pub(crate) steam_app_id: Option<u32>,
    pub(crate) shipping_enabled: bool,
    pub(crate) steam_net_enabled: bool,
    pub(crate) native_enabled: bool,
    pub(crate) dev_hot_reload_enabled: bool,
}

pub(crate) fn validate_release_build(inputs: ReleaseBuildInputs<'_>) -> Result<(), String> {
    validate_build_label(inputs.release_label)?;

    let release_steam_client = inputs.profile == "release" && inputs.steam_net_enabled;
    if release_steam_client {
        validate_release_steam_app_id(inputs.steam_app_id)?;
    }

    let release_shipping = inputs.profile == "release" && inputs.shipping_enabled;
    if !release_shipping {
        return Ok(());
    }
    if inputs.release_label == DEVELOPMENT_BUILD_LABEL {
        return Err(format!(
            "release shipping builds require {BUILD_LABEL_ENV}=<immutable release label>"
        ));
    }
    if !inputs.native_enabled {
        return Err("release shipping builds require Cargo feature `native`".to_owned());
    }
    if !inputs.steam_net_enabled {
        return Err("release shipping builds require Cargo feature `steam-net`".to_owned());
    }
    if inputs.dev_hot_reload_enabled {
        return Err(
            "release shipping builds cannot enable Cargo feature `dev-hot-reload`; use \
             --no-default-features --features shipping"
                .to_owned(),
        );
    }
    validate_release_steam_app_id(inputs.steam_app_id)
}

pub(crate) fn validate_build_label(label: &str) -> Result<(), String> {
    if label.is_empty() || label.len() > MAX_BUILD_LABEL_BYTES {
        return Err(format!(
            "{BUILD_LABEL_ENV} must contain 1..={MAX_BUILD_LABEL_BYTES} ASCII characters"
        ));
    }
    if !label
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(format!(
            "{BUILD_LABEL_ENV} must start with an ASCII letter or digit"
        ));
    }
    if !label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(format!(
            "{BUILD_LABEL_ENV} may contain only ASCII letters, digits, '.', '_', '+', and '-'"
        ));
    }
    Ok(())
}

fn validate_release_steam_app_id(steam_app_id: Option<u32>) -> Result<(), String> {
    let Some(steam_app_id) = steam_app_id else {
        return Err(format!(
            "release Steam client builds require {STEAM_APP_ID_ENV}=<real non-zero Steam App ID>"
        ));
    };
    if steam_app_id == 0 {
        return Err(format!(
            "release Steam client builds require {STEAM_APP_ID_ENV}=<real non-zero Steam App ID>"
        ));
    }
    if steam_app_id == SPACEWAR_APP_ID {
        return Err(format!(
            "release Steam client builds cannot use Spacewar App ID {SPACEWAR_APP_ID}"
        ));
    }
    Ok(())
}

pub(crate) fn parse_steam_app_id(raw: &str) -> Result<u32, String> {
    if raw.is_empty() || raw.len() > 10 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{STEAM_APP_ID_ENV} must be a non-zero decimal u32"));
    }
    let app_id = raw
        .parse::<u32>()
        .map_err(|_| format!("{STEAM_APP_ID_ENV} must be a non-zero decimal u32"))?;
    if app_id == 0 {
        return Err(format!("{STEAM_APP_ID_ENV} must be a non-zero decimal u32"));
    }
    Ok(app_id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamedInput {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
}

impl NamedInput {
    pub(crate) fn new(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            bytes: bytes.into(),
        }
    }
}

pub(crate) fn build_metadata_inputs(
    package: &str,
    version: &str,
    profile: &str,
    configured_build_label: &str,
    steam_app_id: Option<u32>,
    enabled_features: &[String],
) -> Vec<NamedInput> {
    let mut enabled_features = enabled_features.to_vec();
    enabled_features.sort_unstable();
    vec![
        NamedInput::new("@metadata/package", package.as_bytes().to_vec()),
        NamedInput::new("@metadata/version", version.as_bytes().to_vec()),
        NamedInput::new("@metadata/profile", profile.as_bytes().to_vec()),
        NamedInput::new(
            "@metadata/build-label",
            configured_build_label.as_bytes().to_vec(),
        ),
        NamedInput::new(
            "@metadata/steam-app-id",
            steam_app_id
                .map(|app_id| app_id.to_string())
                .unwrap_or_else(|| "development-unconfigured".to_owned())
                .into_bytes(),
        ),
        NamedInput::new(
            "@metadata/cargo-features",
            enabled_features.join("\n").into_bytes(),
        ),
    ]
}

/// Returns every Rust source below `src/` with a platform-neutral relative
/// path. The build identity intentionally covers presentation, application,
/// diagnostics, binaries, and test-only modules too: an added production
/// module cannot silently escape exact-build matchmaking.
pub(crate) fn collect_rust_source_paths(project_root: &Path) -> Result<Vec<String>, String> {
    fn visit(
        project_root: &Path,
        directory: &Path,
        output: &mut Vec<String>,
    ) -> Result<(), String> {
        let entries = fs::read_dir(directory)
            .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("could not enumerate {}: {error}", directory.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "source symlinks are not supported compatibility inputs: {}",
                    path.display()
                ));
            }
            if file_type.is_dir() {
                visit(project_root, &path, output)?;
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            {
                output.push(normalized_relative_path(project_root, &path)?);
            }
        }
        Ok(())
    }

    let source_root = project_root.join("src");
    let mut paths = Vec::new();
    visit(project_root, &source_root, &mut paths)?;
    paths.sort_unstable();
    ensure_unique_paths(&paths)?;
    Ok(paths)
}

pub(crate) fn build_source_inputs(project_root: &Path) -> Result<Vec<NamedInput>, String> {
    let mut paths = ROOT_BUILD_SOURCES
        .iter()
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    paths.extend(collect_rust_source_paths(project_root)?);
    read_named_inputs(project_root, paths)
}

pub(crate) fn gameplay_source_inputs(project_root: &Path) -> Result<Vec<NamedInput>, String> {
    read_named_inputs(
        project_root,
        GAMEPLAY_SOURCES
            .iter()
            .map(|path| (*path).to_owned())
            .collect(),
    )
}

fn read_named_inputs(
    project_root: &Path,
    mut paths: Vec<String>,
) -> Result<Vec<NamedInput>, String> {
    paths.sort_unstable();
    ensure_unique_paths(&paths)?;
    paths
        .into_iter()
        .map(|path| {
            let bytes = fs::read(project_root.join(&path))
                .map_err(|error| format!("failed to read compatibility input {path}: {error}"))?;
            Ok(NamedInput::new(path, canonical_text_bytes(&bytes)))
        })
        .collect()
}

/// Git may check text out with CRLF on Windows even though the committed blob
/// is LF-normalized. Compatibility is source-semantic and cross-platform, so
/// hash one canonical text representation on every host.
pub(crate) fn canonical_text_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            index += 2;
        } else {
            normalized.push(bytes[index]);
            index += 1;
        }
    }
    normalized
}

fn normalized_relative_path(project_root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(project_root)
        .map_err(|_| format!("{} is outside the project root", path.display()))?;
    let relative = relative
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", relative.display()))?;
    Ok(relative.replace('\\', "/"))
}

fn ensure_unique_paths(paths: &[String]) -> Result<(), String> {
    if let Some(duplicate) = paths
        .windows(2)
        .find(|pair| pair[0] == pair[1])
        .map(|pair| pair[0].as_str())
    {
        return Err(format!("duplicate compatibility input {duplicate}"));
    }
    Ok(())
}

fn enabled_cargo_features() -> Vec<String> {
    let mut features = env::vars()
        .filter_map(|(name, _)| name.strip_prefix("CARGO_FEATURE_").map(str::to_owned))
        .collect::<Vec<_>>();
    features.sort_unstable();
    features
}

pub(crate) fn expanded_digest(label: &str, inputs: &[NamedInput], width: usize) -> Vec<u8> {
    assert!(width.is_multiple_of(8));
    let mut inputs = inputs.iter().collect::<Vec<_>>();
    inputs.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    assert!(
        inputs.windows(2).all(|pair| pair[0].name != pair[1].name),
        "compatibility input names must be unique"
    );

    let mut output = vec![0_u8; width];
    for lane in 0..(width / 8) {
        let mut hash = StableHash64::new();
        hash.write_len_prefixed(label.as_bytes());
        hash.write(&[lane as u8]);
        for input in &inputs {
            hash.write_len_prefixed(input.name.as_bytes());
            hash.write_len_prefixed(&input.bytes);
        }
        output[lane * 8..(lane + 1) * 8].copy_from_slice(&hash.finish().to_le_bytes());
    }
    output
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

struct StableHash64(u64);

impl StableHash64 {
    const fn new() -> Self {
        Self(FNV1A_64_OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(FNV1A_64_PRIME);
        }
    }

    fn write_len_prefixed(&mut self, bytes: &[u8]) {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}
