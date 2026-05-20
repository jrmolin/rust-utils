use std::path::{Path, PathBuf};

/// Finds the nearest ancestor directory that contains a `.git` directory or file.
pub fn find_repo_root(start: impl AsRef<Path>) -> Option<PathBuf> {
    let mut current = start.as_ref().to_path_buf();

    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }

    loop {
        let git_path = current.join(".git");

        if git_path.is_dir() || git_path.is_file() {
            return Some(current);
        }

        if !current.pop() {
            return None;
        }
    }
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
