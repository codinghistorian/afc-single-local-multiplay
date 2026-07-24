use std::process::ExitCode;

use ffc_prototype::multiplayer_performance::{
    MULTIPLAYER_PROFILE_HELP, MultiplayerProfileCliAction, parse_multiplayer_profile_args,
    run_multiplayer_allocation_diagnosis, run_multiplayer_profile,
};

fn main() -> ExitCode {
    let action = match parse_multiplayer_profile_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("{error}\n\n{MULTIPLAYER_PROFILE_HELP}");
            return ExitCode::from(2);
        }
    };
    let MultiplayerProfileCliAction::Run(config) = action else {
        print!("{MULTIPLAYER_PROFILE_HELP}");
        return ExitCode::SUCCESS;
    };

    let report_only = config.report_only;
    if config.allocation_breakdown {
        eprintln!(
            "diagnosing production multiplayer allocations: run_id={:?} samples={}",
            config.run_id, config.samples
        );
        return match run_multiplayer_allocation_diagnosis(&config) {
            Ok(result) => {
                println!("{}", result.machine_record());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }

    eprintln!(
        "profiling production headless authority and exact 12-tick rollback: \
         run_id={:?} hardware={:?} samples={}",
        config.run_id, config.hardware, config.samples
    );
    match run_multiplayer_profile(&config) {
        Ok(result) => {
            println!("{}", result.machine_record());
            if result.acceptance_pass {
                eprintln!("multiplayer performance acceptance passed");
                ExitCode::SUCCESS
            } else if report_only {
                eprintln!(
                    "multiplayer performance acceptance failed; --report-only suppresses \
                     the nonzero acceptance exit"
                );
                ExitCode::SUCCESS
            } else {
                eprintln!("multiplayer performance acceptance failed");
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
