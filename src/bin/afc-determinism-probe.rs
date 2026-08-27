use std::process::ExitCode;

fn main() -> ExitCode {
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
