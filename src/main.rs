fn main() -> bevy::app::AppExit {
    use ffc_prototype::release_identity::{
        CommonReleaseCliAction, common_release_cli_action, current_release_identity,
    };

    match common_release_cli_action(std::env::args_os().skip(1)) {
        CommonReleaseCliAction::Version => {
            println!("{}", current_release_identity().version_line());
            return bevy::app::AppExit::Success;
        }
        CommonReleaseCliAction::ReleaseIdentity => {
            println!("{}", current_release_identity().to_deterministic_json());
            return bevy::app::AppExit::Success;
        }
        CommonReleaseCliAction::Run => {}
    }

    if ffc_prototype::native_online::restart_native_steam_release_if_necessary() {
        return bevy::app::AppExit::Success;
    }
    ffc_prototype::build_app().run()
}
