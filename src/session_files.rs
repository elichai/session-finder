use std::fs::{self, ReadDir};
use std::path::{Path, PathBuf};

pub(crate) struct SessionFiles {
    directories: Vec<ReadDir>,
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
                .filter_map(|root| fs::read_dir(root).ok())
                .collect(),
            accepts,
        }
    }
}

impl Iterator for SessionFiles {
    type Item = PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(directory) = self.directories.last_mut() {
            let Some(entry) = directory.next() else {
                self.directories.pop();
                continue;
            };
            let Ok(entry) = entry else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if let Ok(directory) = fs::read_dir(entry.path()) {
                    self.directories.push(directory);
                }
            } else if kind.is_file() {
                let path = entry.path();
                if (self.accepts)(&path) {
                    return Some(path);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

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
