use std::path::{Path, PathBuf};

/// Describes which filesystem entry types can satisfy a root marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkerKind {
    File,
    Directory,
    FileOrDirectory,
}

impl MarkerKind {
    fn matches(self, path: &Path) -> bool {
        match self {
            Self::File => path.is_file(),
            Self::Directory => path.is_dir(),
            Self::FileOrDirectory => path.is_file() || path.is_dir(),
        }
    }
}

/// Finds the nearest ancestor directory that contains `marker` with `kind`.
pub fn find_root(
    start: impl AsRef<Path>,
    marker: impl AsRef<Path>,
    kind: MarkerKind,
) -> Option<PathBuf> {
    let marker = marker.as_ref();
    let mut current = start.as_ref().to_path_buf();

    if marker.as_os_str().is_empty() {
        return None;
    }

    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }

    loop {
        if kind.matches(&current.join(marker)) {
            return Some(current);
        }

        if !current.pop() {
            return None;
        }
    }
}

/// Finds the nearest ancestor directory that contains a `.git` directory or file.
pub fn find_repo_root(start: impl AsRef<Path>) -> Option<PathBuf> {
    find_root(start, ".git", MarkerKind::FileOrDirectory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_repo_with_git_directory() {
        let fixture = Fixture::new("git-dir");
        let repo = fixture.path.join("repo");
        let nested = repo.join("a/b/c");

        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_repo_root(&nested), Some(repo));
    }

    #[test]
    fn finds_repo_with_git_file() {
        let fixture = Fixture::new("git-file");
        let repo = fixture.path.join("worktree");
        let nested = repo.join("src/bin");

        fs::create_dir_all(&nested).unwrap();
        fs::write(
            repo.join(".git"),
            "gitdir: ../main/.git/worktrees/worktree\n",
        )
        .unwrap();

        assert_eq!(find_repo_root(&nested), Some(repo));
    }

    #[test]
    fn returns_none_when_no_repo_marker_exists() {
        let fixture = Fixture::new("missing");
        let nested = fixture.path.join("a/b/c");

        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_repo_root(&nested), None);
    }

    #[test]
    fn finds_root_with_custom_directory_marker() {
        let fixture = Fixture::new("custom-dir");
        let root = fixture.path.join("project");
        let nested = root.join("crates/app/src");

        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            find_root(&nested, ".cargo", MarkerKind::Directory),
            Some(root)
        );
    }

    #[test]
    fn finds_root_with_custom_file_marker() {
        let fixture = Fixture::new("custom-file");
        let root = fixture.path.join("project");
        let nested = root.join("src/bin");

        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();

        assert_eq!(
            find_root(&nested, "Cargo.toml", MarkerKind::File),
            Some(root)
        );
    }

    #[test]
    fn respects_marker_kind() {
        let fixture = Fixture::new("marker-kind");
        let root = fixture.path.join("project");
        let nested = root.join("src");

        fs::create_dir_all(root.join(".config")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_root(&nested, ".config", MarkerKind::File), None);
        assert_eq!(
            find_root(&nested, ".config", MarkerKind::FileOrDirectory),
            Some(root)
        );
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
                "rust-tools-find-repo-{name}-{}-{nanos}",
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
