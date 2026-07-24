//! Stable, user-visible identity for support diagnostics and release evidence.
//!
//! The Steam depot build ID is assigned after upload, so the deterministic
//! command-line identity intentionally reports it as `null`. A running Steam
//! client may enrich an in-memory copy without changing matchmaking identity.

use std::ffi::OsString;

use serde::Serialize;

use crate::match_config::{CURRENT_RNG_SCHEME_VERSION, current_compatibility};
use crate::snapshot::SNAPSHOT_SCHEMA_VERSION;

pub const RELEASE_IDENTITY_SCHEMA_VERSION: u16 = 1;
pub const PRODUCT_NAME: &str = "Animal Fighter Club";

const BUILD_ID_HEX: &str = env!("AFC_COMPILED_BUILD_ID");
const GAMEPLAY_CONTENT_HASH_HEX: &str = env!("AFC_COMPILED_GAMEPLAY_CONTENT_HASH");
const RELEASE_LABEL: &str = env!("AFC_COMPILED_RELEASE_LABEL");
const SHORT_DIGEST_HEX_BYTES: usize = 12;

/// Ordered release metadata serialized verbatim by `--release-identity`.
///
/// Field declaration order is part of the diagnostic JSON byte contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReleaseIdentity {
    pub schema_version: u16,
    pub product_name: &'static str,
    pub package_name: &'static str,
    pub product_version: &'static str,
    pub release_label: &'static str,
    pub shipping: bool,
    pub steam_app_id: Option<u32>,
    pub steam_depot_build_id: Option<i32>,
    pub protocol_version: u16,
    pub simulation_version: u16,
    pub rng_scheme_version: u16,
    pub replay_format_version: u16,
    pub snapshot_schema_version: u16,
    pub compatibility_build_id: &'static str,
    pub gameplay_content_hash: &'static str,
}

impl ReleaseIdentity {
    /// Returns compact JSON with a fixed field order and no host-specific data.
    pub fn to_deterministic_json(&self) -> String {
        serde_json::to_string(self).expect("release identity contains only JSON-safe values")
    }

    pub fn version_line(&self) -> String {
        format!(
            "{} {} ({}; build {}; content {}; protocol {}; simulation {}; rng {}; replay {}; \
             snapshot {})",
            self.product_name,
            self.product_version,
            self.release_label,
            short_digest(self.compatibility_build_id),
            short_digest(self.gameplay_content_hash),
            self.protocol_version,
            self.simulation_version,
            self.rng_scheme_version,
            self.replay_format_version,
            self.snapshot_schema_version,
        )
    }

    /// A title-screen label constructed once during UI setup.
    pub fn short_ui_label(&self) -> String {
        format!(
            "v{} • {} • build {}",
            self.product_version,
            self.release_label,
            short_digest(self.compatibility_build_id)
        )
    }

    /// Adds the post-upload Steam value to runtime diagnostics only.
    pub fn with_steam_depot_build_id(mut self, build_id: Option<i32>) -> Self {
        self.steam_depot_build_id = build_id.filter(|build_id| *build_id > 0);
        self
    }
}

pub fn current_release_identity() -> ReleaseIdentity {
    let compatibility = current_compatibility();
    ReleaseIdentity {
        schema_version: RELEASE_IDENTITY_SCHEMA_VERSION,
        product_name: PRODUCT_NAME,
        package_name: env!("CARGO_PKG_NAME"),
        product_version: env!("CARGO_PKG_VERSION"),
        release_label: RELEASE_LABEL,
        shipping: compiled_shipping(),
        steam_app_id: compiled_steam_app_id(),
        // Steam assigns this only after the exact artifact has been uploaded.
        steam_depot_build_id: None,
        protocol_version: compatibility.protocol.get(),
        simulation_version: compatibility.simulation.get(),
        rng_scheme_version: CURRENT_RNG_SCHEME_VERSION,
        replay_format_version: compatibility.replay.get(),
        snapshot_schema_version: SNAPSHOT_SCHEMA_VERSION,
        compatibility_build_id: BUILD_ID_HEX,
        gameplay_content_hash: GAMEPLAY_CONTENT_HASH_HEX,
    }
}

fn compiled_shipping() -> bool {
    match env!("AFC_COMPILED_SHIPPING") {
        "0" => false,
        "1" => true,
        value => panic!("invalid compiled shipping marker {value:?}"),
    }
}

fn compiled_steam_app_id() -> Option<u32> {
    option_env!("AFC_COMPILED_STEAM_APP_ID").map(|value| {
        value
            .parse::<u32>()
            .ok()
            .filter(|app_id| *app_id != 0)
            .expect("build.rs emits a non-zero decimal Steam App ID")
    })
}

fn short_digest(digest: &str) -> &str {
    digest.get(..SHORT_DIGEST_HEX_BYTES).unwrap_or(digest)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommonReleaseCliAction {
    Run,
    Version,
    ReleaseIdentity,
}

/// Recognizes diagnostic flags only when they are the complete argument list.
///
/// Steam launch arguments such as `+connect_lobby <id>` therefore pass through
/// to the normal application startup unchanged.
pub fn common_release_cli_action<I, S>(args: I) -> CommonReleaseCliAction
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let Some(argument) = args.next() else {
        return CommonReleaseCliAction::Run;
    };
    if args.next().is_some() {
        return CommonReleaseCliAction::Run;
    }
    if argument == "--version" {
        CommonReleaseCliAction::Version
    } else if argument == "--release-identity" {
        CommonReleaseCliAction::ReleaseIdentity
    } else {
        CommonReleaseCliAction::Run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ReleaseIdentity {
        ReleaseIdentity {
            schema_version: 1,
            product_name: "Animal Fighter Club",
            package_name: "ffc-prototype",
            product_version: "1.2.3",
            release_label: "steam-rc.4+abc123",
            shipping: true,
            steam_app_id: Some(123_456),
            steam_depot_build_id: None,
            protocol_version: 1,
            simulation_version: 5,
            rng_scheme_version: 1,
            replay_format_version: 1,
            snapshot_schema_version: 2,
            compatibility_build_id: "00112233445566778899aabbccddeeff",
            gameplay_content_hash: "ffeeddccbbaa9988776655443322110000112233445566778899aabbccddeeff",
        }
    }

    #[test]
    fn release_identity_json_has_an_exact_stable_schema_and_field_order() {
        assert_eq!(
            fixture().to_deterministic_json(),
            concat!(
                r#"{"schema_version":1,"product_name":"Animal Fighter Club","#,
                r#""package_name":"ffc-prototype","product_version":"1.2.3","#,
                r#""release_label":"steam-rc.4+abc123","shipping":true,"#,
                r#""steam_app_id":123456,"steam_depot_build_id":null,"#,
                r#""protocol_version":1,"simulation_version":5,"rng_scheme_version":1,"#,
                r#""replay_format_version":1,"snapshot_schema_version":2,"#,
                r#""compatibility_build_id":"00112233445566778899aabbccddeeff","#,
                r#""gameplay_content_hash":"ffeeddccbbaa9988776655443322110000112233445566778899aabbccddeeff"}"#
            )
        );
    }

    #[test]
    fn current_identity_matches_the_compiled_compatibility_contract() {
        let identity = current_release_identity();
        let compatibility = current_compatibility();
        assert_eq!(identity.protocol_version, compatibility.protocol.get(),);
        assert_eq!(identity.simulation_version, compatibility.simulation.get(),);
        assert_eq!(identity.replay_format_version, compatibility.replay.get(),);
        assert_eq!(identity.rng_scheme_version, CURRENT_RNG_SCHEME_VERSION);
        assert_eq!(identity.snapshot_schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(identity.compatibility_build_id.len(), 32);
        assert_eq!(identity.gameplay_content_hash.len(), 64);
        assert!(
            identity
                .compatibility_build_id
                .bytes()
                .chain(identity.gameplay_content_hash.bytes())
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(identity.steam_depot_build_id, None);
    }

    #[test]
    fn diagnostic_cli_flags_are_exact_and_standalone_only() {
        assert_eq!(
            common_release_cli_action(["--version"]),
            CommonReleaseCliAction::Version
        );
        assert_eq!(
            common_release_cli_action(["--release-identity"]),
            CommonReleaseCliAction::ReleaseIdentity
        );
        for args in [
            Vec::<&str>::new(),
            vec!["--unknown"],
            vec!["--version", "extra"],
            vec!["+connect_lobby", "123456"],
            vec!["--release-identity", "+connect_lobby", "123456"],
        ] {
            assert_eq!(common_release_cli_action(args), CommonReleaseCliAction::Run);
        }
    }

    #[test]
    fn support_labels_are_short_and_depot_id_is_runtime_only() {
        let identity = fixture();
        assert_eq!(
            identity.short_ui_label(),
            "v1.2.3 • steam-rc.4+abc123 • build 001122334455"
        );
        assert_eq!(
            identity.version_line(),
            "Animal Fighter Club 1.2.3 (steam-rc.4+abc123; build 001122334455; content \
             ffeeddccbbaa; protocol 1; simulation 5; rng 1; replay 1; snapshot 2)"
        );
        assert_eq!(
            identity
                .clone()
                .with_steam_depot_build_id(Some(42))
                .steam_depot_build_id,
            Some(42)
        );
        assert_eq!(
            identity
                .with_steam_depot_build_id(Some(0))
                .steam_depot_build_id,
            None
        );
    }
}
