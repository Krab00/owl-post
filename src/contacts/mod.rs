//! Contact book: the two scopes merged, policy overlay, lookup. See docs/technical-design.md §5.
//!
//! Scope names follow the user's view: **global** = `$OWLPOST_HOME/contacts/` (this machine,
//! every repo; module [`local`]), **local** = `<git root>/.agents/peers/` (this repository,
//! shared via PR; module [`repo`]).

pub mod local;
pub mod repo;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::identity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// May be empty in a policy overlay file (only `pubkey` + `policy` matter there).
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub emails: Vec<String>,
    pub pubkey: String,
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// `"global"` (`$OWLPOST_HOME/contacts/`) or `"local"` (`.agents/peers/`); set by the
    /// provider, not trusted from the file.
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// Derived from `pubkey` at load time; never read from the file.
    #[serde(skip_deserializing)]
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub mode: Mode,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_hour: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Manual,
    Auto,
    Never,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::Auto => "auto",
            Mode::Never => "never",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub projects: Vec<String>,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            projects: vec!["*".into()],
        }
    }
}

impl Contact {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Parse one contact file; a bad pubkey counts as malformed. A local (repo) file never
/// carries policy: anyone who lands a commit could otherwise grant themselves `auto`, so its
/// `policy` is dropped and only the global store (`owl allow` / `owl deny`) sets one.
fn parse(path: &Path, source: &str) -> anyhow::Result<Contact> {
    let bytes = std::fs::read(path)?;
    let mut c: Contact = serde_json::from_slice(&bytes)?;
    let pk = identity::parse_pubkey(&c.pubkey)?;
    c.fingerprint = identity::fingerprint(&pk);
    c.source = source.to_string();
    if source == "local" {
        c.policy = None;
    }
    Ok(c)
}

/// Load every `*.json` in `dir`; malformed files are skipped with a warning on stderr.
fn load_dir(dir: &Path, source: &str) -> Vec<Contact> {
    load_dir_paths(dir, source)
        .into_iter()
        .map(|(_, c)| c)
        .collect()
}

/// [`load_dir`] keeping each contact's file path (for `owl contact remove`).
fn load_dir_paths(dir: &Path, source: &str) -> Vec<(PathBuf, Contact)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        match parse(&p, source) {
            Ok(c) => out.push((p, c)),
            Err(e) => eprintln!("owl: warning: skipping {}: {e:#}", p.display()),
        }
    }
    out
}

/// Fold a second file for the same key into `into`, whatever order the files were read in:
/// a policy wins over no policy, and a contact file (non-empty `name`) supplies the contact
/// fields when `into` is a bare policy overlay.
fn merge(into: &mut Contact, other: Contact) {
    if other.policy.is_some() {
        into.policy = other.policy;
    }
    if into.name.is_empty() && !other.name.is_empty() {
        into.name = other.name;
        into.emails = other.emails;
        into.endpoints = other.endpoints;
        into.added_at = other.added_at;
    }
}

#[derive(Debug, Default, Clone)]
pub struct ContactBook {
    pub contacts: Vec<Contact>,
}

impl ContactBook {
    /// Local (repo) contacts first (from the git root above `cwd`), then global ones. A global
    /// file whose pubkey matches a local contact contributes only its `policy`.
    pub fn load(home: &Path, cwd: &Path) -> anyhow::Result<ContactBook> {
        let mut contacts = repo::find_git_root(cwd)
            .map(|r| repo::load(&r))
            .unwrap_or_default();
        for l in local::load(home) {
            match contacts.iter_mut().find(|c| c.pubkey == l.pubkey) {
                Some(existing) => merge(existing, l),
                None => contacts.push(l),
            }
        }
        Ok(ContactBook { contacts })
    }

    /// The merged book restricted to contacts whose `source` is `scope` (`"global"` or
    /// `"local"`); a policy overlay stays with the contact it belongs to.
    pub fn load_scope(home: &Path, cwd: &Path, scope: &str) -> anyhow::Result<ContactBook> {
        let mut book = ContactBook::load(home, cwd)?;
        book.contacts.retain(|c| c.source == scope);
        Ok(book)
    }

    /// `owl contact remove`: resolve `query` among the contacts of `scope` only, then delete
    /// every file holding that key in the scope — and, for both scopes, its policy overlay in
    /// `$home/contacts/` (the overlay is policy, not a contact; leaving it would resurface as a
    /// nameless global contact). Returns the contact's name and the paths removed.
    pub fn remove(
        home: &Path,
        cwd: &Path,
        scope: &str,
        query: &str,
    ) -> anyhow::Result<(String, Vec<PathBuf>)> {
        let root = if scope == "local" {
            Some(
                repo::find_git_root(cwd)
                    .with_context(|| format!("{} is not inside a git repository", cwd.display()))?,
            )
        } else {
            None
        };
        let book = ContactBook::load_scope(home, cwd, scope)?;
        let c = book.resolve(query)?;
        let (fp, name) = (c.fingerprint.clone(), c.name.clone());
        let mut paths = Vec::new();
        if let Some(root) = root {
            paths.extend(
                load_dir_paths(&repo::peers_dir(&root), "local")
                    .into_iter()
                    .filter(|(_, c)| c.fingerprint == fp)
                    .map(|(p, _)| p),
            );
        }
        paths.extend(
            load_dir_paths(&local::dir(home), "global")
                .into_iter()
                .filter(|(_, c)| c.fingerprint == fp)
                .map(|(p, _)| p),
        );
        for p in &paths {
            std::fs::remove_file(p).with_context(|| format!("removing {}", p.display()))?;
        }
        Ok((name, paths))
    }

    /// `(uri, contact)` per contact, sorted by name (then fingerprint) so the list is stable.
    /// URI: `to://<slug>.<first e-mail>` (`to://<slug>` without e-mail); when two contacts share
    /// a URI each gets `.<fingerprint without owl:>` appended, so every URI is unique. `owl mcp`
    /// serves these as MCP resources (§9) and [`resolve`](Self::resolve) accepts one as a peer.
    pub fn uris(&self) -> Vec<(String, &Contact)> {
        let mut contacts: Vec<&Contact> = self.contacts.iter().collect();
        contacts.sort_by(|a, b| (&a.name, &a.fingerprint).cmp(&(&b.name, &b.fingerprint)));
        let bases: Vec<String> = contacts.iter().map(|c| base_uri(c)).collect();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for b in &bases {
            *counts.entry(b.as_str()).or_default() += 1;
        }
        contacts
            .into_iter()
            .zip(bases.iter())
            .map(|(c, base)| {
                let uri = if counts[base.as_str()] > 1 {
                    format!("{base}.{}", fp_segment(c))
                } else {
                    base.clone()
                };
                (uri, c)
            })
            .collect()
    }

    /// The contact whose [`uris`](Self::uris) entry is exactly `query` — an `@owl:to://…`
    /// mention as Claude Code passes it in a slash command's arguments, or the bare `to://…`.
    /// A URI is exact by construction, so there is no prefix fallback; `None` lets
    /// [`resolve`](Self::resolve) fall through to the name arm.
    fn uri_match(&self, query: &str) -> Option<&Contact> {
        let uri = query.strip_prefix("@owl:").unwrap_or(query);
        if !uri.starts_with("to://") {
            return None;
        }
        self.uris()
            .into_iter()
            .find(|(u, _)| u == uri)
            .map(|(_, c)| c)
    }

    /// Exact fingerprint → exact email → exact `owl mcp` resource URI (`@owl:to://…` /
    /// `to://…`) → unique case-insensitive name prefix.
    pub fn resolve(&self, query: &str) -> anyhow::Result<&Contact> {
        if let Some(c) = self.contacts.iter().find(|c| c.fingerprint == query) {
            return Ok(c);
        }
        if let Some(c) = self
            .contacts
            .iter()
            .find(|c| c.emails.iter().any(|e| e == query))
        {
            return Ok(c);
        }
        if let Some(c) = self.uri_match(query) {
            return Ok(c);
        }
        let q = query.to_lowercase();
        let hits: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&q))
            .collect();
        match hits.as_slice() {
            [one] => Ok(one),
            [] => bail!("no contact matches {query:?}"),
            many => {
                let names: Vec<&str> = many.iter().map(|c| c.name.as_str()).collect();
                bail!("ambiguous peer {query:?}: matches {}", names.join(", "))
            }
        }
    }

    pub fn policy_for(&self, fingerprint: &str) -> Option<&Policy> {
        self.contacts
            .iter()
            .find(|c| c.fingerprint == fingerprint)
            .and_then(|c| c.policy.as_ref())
    }

    /// Write/update the overlay `$home/contacts/<fingerprint>.json`. For a local (repo) contact
    /// the file holds only pubkey + policy (+ source/added_at); an existing file keeps its fields.
    pub fn set_policy(
        &mut self,
        home: &Path,
        fingerprint: &str,
        policy: Policy,
    ) -> anyhow::Result<()> {
        let contact = self
            .contacts
            .iter_mut()
            .find(|c| c.fingerprint == fingerprint)
            .with_context(|| format!("no contact with fingerprint {fingerprint}"))?;
        let dir = local::dir(home);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{fingerprint}.json"));
        let mut v: serde_json::Value = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(_) if contact.source == "local" => serde_json::json!({
                "pubkey": contact.pubkey,
                "source": "global",
                "added_at": rfc3339(now_secs()),
            }),
            Err(_) => {
                let mut full = contact.clone();
                full.added_at.get_or_insert_with(|| rfc3339(now_secs()));
                serde_json::to_value(&full)?
            }
        };
        let obj = v
            .as_object_mut()
            .with_context(|| format!("{} is not a JSON object", path.display()))?;
        obj.insert("policy".into(), serde_json::to_value(&policy)?);
        obj.remove("fingerprint");
        std::fs::write(&path, serde_json::to_vec_pretty(&v)?)?;
        contact.policy = Some(policy);
        Ok(())
    }
}

fn base_uri(c: &Contact) -> String {
    let mut segments: Vec<String> = Vec::new();
    let slug = slug(&c.name);
    if !slug.is_empty() {
        segments.push(slug);
    }
    if let Some(email) = c.emails.first().filter(|e| !e.is_empty()) {
        segments.push(email.clone());
    }
    if segments.is_empty() {
        // A nameless, e-mail-less contact (a stray policy overlay): the fingerprint alone.
        segments.push(fp_segment(c).to_string());
    }
    format!("to://{}", segments.join("."))
}

fn fp_segment(c: &Contact) -> &str {
    c.fingerprint.strip_prefix("owl:").unwrap_or(&c.fingerprint)
}

/// Name lower-cased, every run of non-alphanumerics (Unicode: `ë` and `ł` stay) as one `-`,
/// no leading or trailing `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// How a signing key stands in the contact book (§5). A name is a label; the key is
/// the identity, so every consent presentation says how the key got into the book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStanding {
    /// The fingerprint is in the merged book; `source` is the merged scope (`local` wins).
    Contact { name: String, source: String },
    /// No contact file holds this fingerprint any more (the contact was removed).
    Unknown,
}

impl KeyStanding {
    /// The `owl inbox --json` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyStanding::Contact { .. } => "contact",
            KeyStanding::Unknown => "unknown",
        }
    }

    /// The line a human reads. A contact with an empty `name` (a bare policy overlay) shows
    /// `fp` in place of the name.
    pub fn wording(&self, fp: &str) -> String {
        match self {
            KeyStanding::Contact { name, source } => {
                let shown = if name.is_empty() { fp } else { name.as_str() };
                if source == "local" {
                    format!(
                        "known key: contact \"{shown}\" — repo peer file (.agents/peers/, reviewed in a PR)"
                    )
                } else {
                    format!(
                        "known key: contact \"{shown}\" — added by hand (global book; fingerprint not verified through a PR)"
                    )
                }
            }
            KeyStanding::Unknown => {
                "unknown key: no longer in your contacts — deny, or re-add the peer file before allowing"
                    .to_string()
            }
        }
    }
}

/// The standing of `fp` in the merged `book`.
pub fn key_standing(book: &ContactBook, fp: &str) -> KeyStanding {
    book.contacts
        .iter()
        .find(|c| c.fingerprint == fp)
        .map_or(KeyStanding::Unknown, |c| KeyStanding::Contact {
            name: c.name.clone(),
            source: c.source.clone(),
        })
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ponytail: hand-rolled UTC formatting (Hinnant's civil_from_days) instead of a chrono dep.
fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: &str, emails: &[&str], fp: &str) -> Contact {
        Contact {
            name: name.into(),
            emails: emails.iter().map(|e| e.to_string()).collect(),
            pubkey: String::new(),
            endpoints: vec![],
            source: "global".into(),
            policy: None,
            added_at: None,
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn slug_rules() {
        assert_eq!(slug("Ana Kowalska"), "ana-kowalska");
        assert_eq!(slug("  Zoë O'Brien-Łukasz!! "), "zoë-o-brien-łukasz");
        assert_eq!(slug("--"), "");
        assert_eq!(slug(""), "");
        assert_eq!(slug("a__b  c"), "a-b-c");
    }

    #[test]
    fn uris_are_sorted_and_unique() {
        let book = ContactBook {
            contacts: vec![
                contact("Bob", &["bob@x.io"], "owl:bbbb"),
                contact("Ana", &["ana@x.io"], "owl:aaaa"),
                contact("Bob", &["bob@x.io"], "owl:aaab"),
                contact("", &[], "owl:zzzz"),
                contact("No Mail", &[], "owl:nnnn"),
            ],
        };
        let uris: Vec<String> = book.uris().into_iter().map(|(u, _)| u).collect();
        assert_eq!(
            uris,
            [
                "to://zzzz",
                "to://ana.ana@x.io",
                "to://bob.bob@x.io.aaab",
                "to://bob.bob@x.io.bbbb",
                "to://no-mail",
            ]
        );
    }

    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_788_256_800), "2026-09-01T10:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn mode_roundtrip() {
        for (m, s) in [
            (Mode::Manual, "\"manual\""),
            (Mode::Auto, "\"auto\""),
            (Mode::Never, "\"never\""),
        ] {
            assert_eq!(serde_json::to_string(&m).unwrap(), s);
            assert_eq!(serde_json::from_str::<Mode>(s).unwrap(), m);
        }
        assert!(serde_json::from_str::<Mode>("\"Auto\"").is_err());
    }

    #[test]
    fn scope_defaults_to_star() {
        let p: Policy = serde_json::from_str(r#"{"mode":"auto"}"#).unwrap();
        assert_eq!(p.scope.projects, ["*"]);
        assert_eq!(p.rate_limit_per_hour, None);
    }
}
