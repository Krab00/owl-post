//! Local scope (source `"local"`): `<git root>/.agents/peers/*.json`, shared with the team
//! via PR. The module keeps its historical name; the user-facing scope is "local".

use std::path::{Path, PathBuf};

use super::{Contact, load_dir};

/// Walk up from `cwd` to the first directory containing `.git` (dir or worktree file).
pub fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

pub fn peers_dir(root: &Path) -> PathBuf {
    root.join(".agents").join("peers")
}

pub fn load(root: &Path) -> Vec<Contact> {
    load_dir(&peers_dir(root), "local")
}
