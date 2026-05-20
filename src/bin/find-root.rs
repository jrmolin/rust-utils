use std::process::ExitCode;

fn main() -> ExitCode {
    let Ok(current_dir) = std::env::current_dir() else {
        return ExitCode::FAILURE;
    };

    match rust_tools::find_repo_root(current_dir) {
        Some(repo_root) => {
            println!("{}", repo_root.display());
            ExitCode::SUCCESS
        }
        None => ExitCode::FAILURE,
    }
}
