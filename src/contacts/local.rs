//! Global scope (source `"global"`): `$OWLPOST_HOME/contacts/*.json`, full contacts or policy
//! overlays. The module keeps its historical name; the user-facing scope is "global".

use std::path::{Path, PathBuf};

use super::{Contact, load_dir};

pub fn dir(home: &Path) -> PathBuf {
    home.join("contacts")
}

pub fn load(home: &Path) -> Vec<Contact> {
    load_dir(&dir(home), "global")
}
