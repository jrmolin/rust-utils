use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::var_os("PATH") else {
        eprintln!("PATH is not set");
        return ExitCode::FAILURE;
    };

    print!("{}", format_path(path));
    ExitCode::SUCCESS
}

fn format_path(path: impl AsRef<OsStr>) -> String {
    let entries: Vec<_> = std::env::split_paths(path.as_ref()).collect();
    format_entries(&entries)
}

fn format_entries(entries: &[PathBuf]) -> String {
    if entries.is_empty() {
        return "PATH is empty\n".to_string();
    }

    let width = entries.len().to_string().len();
    let mut first_seen = HashMap::new();
    let mut output = String::from("PATH entries:\n");

    for (index, entry) in entries.iter().enumerate() {
        let number = index + 1;
        let duplicate_of = match first_seen.get(entry).copied() {
            Some(first_number) => format!(" ({first_number})"),
            None => {
                first_seen.insert(entry, number);
                String::new()
            }
        };

        writeln!(
            output,
            "{number:>width$}. {entry}{duplicate_of}",
            width = width,
            entry = entry.display()
        )
        .unwrap();
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_empty_path() {
        assert_eq!(format_entries(&[]), "PATH is empty\n");
    }

    #[test]
    fn formats_single_digit_entries() {
        assert_eq!(
            format_entries(&paths(["usr-local-bin", "usr-bin", "bin"])),
            "\
PATH entries:
1. usr-local-bin
2. usr-bin
3. bin
"
        );
    }

    #[test]
    fn aligns_double_digit_entries() {
        assert_eq!(
            format_entries(&paths([
                "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09", "p10",
            ])),
            "\
PATH entries:
 1. p01
 2. p02
 3. p03
 4. p04
 5. p05
 6. p06
 7. p07
 8. p08
 9. p09
10. p10
"
        );
    }

    #[test]
    fn annotates_duplicate_entries_with_first_entry_number() {
        assert_eq!(
            format_entries(&paths([
                "usr-local-bin",
                "usr-bin",
                "bin",
                "usr-bin",
                "cargo-bin",
                "usr-bin",
                "usr-local-bin",
            ])),
            "\
PATH entries:
1. usr-local-bin
2. usr-bin
3. bin
4. usr-bin (2)
5. cargo-bin
6. usr-bin (2)
7. usr-local-bin (1)
"
        );
    }

    #[test]
    fn splits_path_environment_value() {
        let joined = std::env::join_paths(paths(["usr-local-bin", "usr-bin"])).unwrap();

        assert_eq!(
            format_path(joined),
            "\
PATH entries:
1. usr-local-bin
2. usr-bin
"
        );
    }

    fn paths<const N: usize>(entries: [&str; N]) -> Vec<PathBuf> {
        entries.into_iter().map(PathBuf::from).collect()
    }
}
