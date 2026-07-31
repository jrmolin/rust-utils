use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: path-find executable

Print all locations in PATH containing the executable.
";

fn main() -> ExitCode {
    let Some(executable) = std::env::args_os().nth(1) else {
        print!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let Some(path) = std::env::var_os("PATH") else {
        return ExitCode::FAILURE;
    };

    let matches = find_all(path, executable);
    for path in &matches {
        println!("{}", path.display());
    }

    if matches.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn find_all(path: impl AsRef<OsStr>, executable: impl AsRef<Path>) -> Vec<PathBuf> {
    std::env::split_paths(path.as_ref())
        .map(|directory| directory.join(executable.as_ref()))
        .filter(|candidate| is_executable(candidate))
        .collect()
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_executables_in_path_order() {
        let fixture = Fixture::new("ordered");
        let first = fixture.path.join("first");
        let second = fixture.path.join("second");
        let missing = fixture.path.join("missing");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::create_dir_all(&missing).unwrap();

        let first_match = create_executable(&first, "tool");
        let second_match = create_executable(&second, "tool");
        let path = std::env::join_paths([&first, &missing, &second]).unwrap();

        assert_eq!(
            find_all(path, OsString::from("tool")),
            vec![first_match, second_match]
        );
    }

    #[test]
    fn ignores_directories_with_the_requested_name() {
        let fixture = Fixture::new("directory");
        let directory = fixture.path.join("bin");
        fs::create_dir_all(directory.join("tool")).unwrap();
        let path = std::env::join_paths([directory]).unwrap();

        assert!(find_all(path, OsString::from("tool")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn ignores_files_without_execute_permission() {
        let fixture = Fixture::new("not-executable");
        let directory = fixture.path.join("bin");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("tool"), "").unwrap();
        let path = std::env::join_paths([directory]).unwrap();

        assert!(find_all(path, OsString::from("tool")).is_empty());
    }

    fn create_executable(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, "").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        path
    }

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "rust-tools-path-find-{name}-{}-{nanos}",
                std::process::id()
            ));

            fs::create_dir_all(&path).unwrap();

            Self { path }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).ok();
        }
    }
}
