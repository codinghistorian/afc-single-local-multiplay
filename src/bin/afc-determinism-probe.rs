use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let action = args.next();
    if args.next().is_some() || !matches!(action.as_deref(), None | Some("--release-identity")) {
        eprintln!("usage: afc-determinism-probe [--release-identity]");
        return ExitCode::from(2);
    }
    if action.as_deref() == Some("--release-identity") {
        println!(
            "{}",
            ffc_prototype::release_identity::current_release_identity().to_deterministic_json()
        );
        return ExitCode::SUCCESS;
    }
    match ffc_prototype::determinism_probe::run_cross_target_probe_json() {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cross-target determinism probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}
