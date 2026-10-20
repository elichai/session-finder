use std::fs::{self, ReadDir};
use std::path::{Path, PathBuf};

pub(crate) struct SessionFiles {
    directories: Vec<PathBuf>,
    current: Option<ReadDir>,
    accepts: fn(&Path) -> bool,
}

impl SessionFiles {
    pub fn new(root: &Path, accepts: fn(&Path) -> bool) -> Self {
        Self::from_dirs([root], accepts)
    }

    pub fn from_dirs<P: AsRef<Path>>(
        roots: impl IntoIterator<Item = P>,
        accepts: fn(&Path) -> bool,
    ) -> Self {
        Self {
            directories: roots
                .into_iter()
                .map(|root| root.as_ref().to_path_buf())
                .collect(),
            current: None,
            accepts,
        }
    }
}

impl Iterator for SessionFiles {
    type Item = PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let Some(directory) = self.current.as_mut() else {
                self.current = fs::read_dir(self.directories.pop()?).ok();
                continue;
            };
            let Some(entry) = directory.next() else {
                self.current = None;
                continue;
            };
            let Ok(entry) = entry else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                self.directories.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                if (self.accepts)(&path) {
                    return Some(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::process::Command;

    #[test]
    fn walks_many_roots_and_deep_trees_with_a_low_open_file_limit() {
        if std::env::var_os("SESSION_FILES_LOW_FD_CHILD").is_none() {
            let root =
                std::env::temp_dir().join(format!("session-files-limits-{}", std::process::id()));
            let output = Command::new("sh")
                .args([
                    "-c",
                    "set -e\nulimit -n 64\nexec \"$1\" --exact \"$2\" --nocapture",
                    "sh",
                ])
                .arg(std::env::current_exe().unwrap())
                .arg("session_files::tests::walks_many_roots_and_deep_trees_with_a_low_open_file_limit")
                .env("SESSION_FILES_LOW_FD_CHILD", &root)
                .output()
                .unwrap();
            fs::remove_dir_all(root).unwrap();
            assert!(
                output.status.success(),
                "child failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let root = PathBuf::from(std::env::var_os("SESSION_FILES_LOW_FD_CHILD").unwrap());
        let mut roots = Vec::new();
        for index in 0..160 {
            let directory = root.join(format!("roots/{index}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("session.jsonl"), "").unwrap();
            roots.push(directory);
        }
        let roots_count = SessionFiles::from_dirs(roots, |_| true).count();

        let mut directory = root.join("tree");
        for _ in 0..160 {
            directory.push("d");
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("session.jsonl"), "").unwrap();
        }
        for index in 0..160 {
            let directory = root.join(format!("tree/wide-{index}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("session.jsonl"), "").unwrap();
        }
        let tree_count = SessionFiles::new(&root.join("tree"), |_| true).count();
        assert_eq!((roots_count, tree_count), (160, 320));
    }

    #[test]
    fn walks_nested_files_without_following_symlinks_or_matching_other_extensions() {
        let root = std::env::temp_dir().join(format!("session-files-{}", std::process::id()));
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        for name in [
            "first.jsonl",
            "nested/second.jsonl",
            "nested/deep/other.txt",
        ] {
            fs::write(root.join(name), "").unwrap();
        }
        symlink(&root, root.join("nested/cycle")).unwrap();
        symlink(root.join("first.jsonl"), root.join("linked.jsonl")).unwrap();
        let mut found: Vec<_> = SessionFiles::new(&root, |path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
        found.sort();
        assert_eq!(
            found,
            [root.join("first.jsonl"), root.join("nested/second.jsonl")]
        );
        assert_eq!(
            SessionFiles::new(&root.join("missing"), |_| true).count(),
            0
        );
        fs::remove_dir_all(root).unwrap();
    }
}
