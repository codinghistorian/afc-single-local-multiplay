use std::process::ExitCode;

use ffc_prototype::dedicated_server::{
    DEDICATED_HELP, DedicatedCliAction, parse_dedicated_args, run_standalone_dedicated,
};
use ffc_prototype::release_identity::current_release_identity;

fn main() -> ExitCode {
    let args = match std::env::args().skip(1).collect::<Vec<_>>() {
        args => args,
    };
    let action = match parse_dedicated_args(args) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("{error}\n\n{DEDICATED_HELP}");
            return ExitCode::from(2);
        }
    };
    let options = match action {
        DedicatedCliAction::Help => {
            print!("{DEDICATED_HELP}");
            return ExitCode::SUCCESS;
        }
        DedicatedCliAction::Version => {
            println!("{}", current_release_identity().version_line());
            return ExitCode::SUCCESS;
        }
        DedicatedCliAction::ReleaseIdentity => {
            println!("{}", current_release_identity().to_deterministic_json());
            return ExitCode::SUCCESS;
        }
        DedicatedCliAction::Run(options) => options,
    };

    let match_id = options.match_id;
    let manifest_seed = options.master_seed;
    eprintln!(
        "starting deployment/test-only render-free 60 Hz bot authority: \
         match={match_id:?} seed={manifest_seed}; hosted Steam SDR and trusted results are disabled"
    );
    match run_standalone_dedicated(options) {
        Ok(terminal) => {
            eprintln!(
                "dedicated authority stopped: exit={:?} tick={} simulated_ticks={} max_step_ns={}",
                terminal.exit,
                terminal.last_tick.get(),
                terminal.metrics.simulated_ticks,
                terminal.metrics.maximum_step_duration_ns,
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dedicated authority failed: {error}");
            ExitCode::FAILURE
        }
    }
}
