use std::process::ExitCode;

fn main() -> ExitCode {
    match deckard_host_tools::shared_folders::run_hook() {
        Ok(report) => {
            for warning in report.warnings {
                eprintln!("deckard-shared-folders: {warning}");
            }
            if !report.shared.is_empty() {
                eprintln!(
                    "deckard-shared-folders: shared {}",
                    report.shared.join(", ")
                );
            }
        }
        Err(error) => eprintln!("deckard-shared-folders: {error}"),
    }
    ExitCode::SUCCESS
}
