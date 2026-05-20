use std::path::PathBuf;
use std::process::ExitCode;

use rust_tools::MarkerKind;

const DEFAULT_MARKER: &str = ".git";
const USAGE: &str = "\
Usage: find-root [-f|-d|-fd] [marker]

Finds the nearest ancestor of the current directory containing marker.

Options:
  -f    marker must be a file
  -d    marker must be a directory
  -fd   marker may be a file or directory
";

fn main() -> ExitCode {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(Args::Run(config)) => config,
        Ok(Args::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let Ok(current_dir) = std::env::current_dir() else {
        return ExitCode::FAILURE;
    };

    match rust_tools::find_root(current_dir, config.marker, config.kind) {
        Some(repo_root) => {
            println!("{}", repo_root.display());
            ExitCode::SUCCESS
        }
        None => ExitCode::FAILURE,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Config {
    marker: PathBuf,
    kind: MarkerKind,
}

#[derive(Debug, Eq, PartialEq)]
enum Args {
    Run(Config),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut marker = None;
    let mut kind = MarkerKind::FileOrDirectory;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Args::Help),
            "-f" => kind = MarkerKind::File,
            "-d" => kind = MarkerKind::Directory,
            "-fd" => kind = MarkerKind::FileOrDirectory,
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
            _ if marker.is_some() => return Err(format!("unexpected argument: {arg}")),
            _ => marker = Some(PathBuf::from(arg)),
        }
    }

    Ok(Args::Run(Config {
        marker: marker.unwrap_or_else(|| PathBuf::from(DEFAULT_MARKER)),
        kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_git_marker_with_file_or_directory_kind() {
        assert_eq!(
            parse([]),
            Ok(Args::Run(Config {
                marker: PathBuf::from(DEFAULT_MARKER),
                kind: MarkerKind::FileOrDirectory,
            }))
        );
    }

    #[test]
    fn accepts_file_marker() {
        assert_eq!(
            parse(["-f", "Cargo.toml"]),
            Ok(Args::Run(Config {
                marker: PathBuf::from("Cargo.toml"),
                kind: MarkerKind::File,
            }))
        );
    }

    #[test]
    fn accepts_directory_marker() {
        assert_eq!(
            parse(["-d", ".hg"]),
            Ok(Args::Run(Config {
                marker: PathBuf::from(".hg"),
                kind: MarkerKind::Directory,
            }))
        );
    }

    #[test]
    fn accepts_file_or_directory_marker() {
        assert_eq!(
            parse(["-fd", ".git"]),
            Ok(Args::Run(Config {
                marker: PathBuf::from(".git"),
                kind: MarkerKind::FileOrDirectory,
            }))
        );
    }

    #[test]
    fn accepts_marker_before_kind() {
        assert_eq!(
            parse(["Cargo.toml", "-f"]),
            Ok(Args::Run(Config {
                marker: PathBuf::from("Cargo.toml"),
                kind: MarkerKind::File,
            }))
        );
    }

    #[test]
    fn rejects_unknown_option() {
        assert!(parse(["--bad"]).is_err());
    }

    #[test]
    fn rejects_multiple_markers() {
        assert!(parse(["Cargo.toml", "package.json"]).is_err());
    }

    fn parse<const N: usize>(args: [&str; N]) -> Result<Args, String> {
        parse_args(args.into_iter().map(String::from))
    }
}
