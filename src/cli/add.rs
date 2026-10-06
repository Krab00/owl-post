//! `owl add <peer-file | '<json>' | -> [--local]`: validate a peer file and write it as
//! `<slug>.json` into the global book (`$OWLPOST_HOME/contacts/`) or, with `--local`, into
//! `<git root>/.agents/peers/`. TOFU (architecture §4): adding never sets a policy.

use std::io::Read;
use std::path::Path;

use anyhow::Context;
use owlpost::contacts::{ContactBook, local, repo};
use owlpost::daemon::write_atomic;
use owlpost::identity;
use serde_json::{Map, Value};

use super::user_error;

pub fn run(home: &Path, source: &str, local: bool) -> anyhow::Result<()> {
    let text = read_source(source)?;
    let peer = validate(&text)?;
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let book = ContactBook::load(home, &cwd)?;
    if let Some(c) = book
        .contacts
        .iter()
        .find(|c| c.fingerprint == peer.fingerprint)
    {
        return Err(user_error(format!(
            "already a contact: {} ({})",
            c.name, c.source
        )));
    }
    let (dir, scope) = if local {
        let root = repo::find_git_root(&cwd).ok_or_else(|| {
            user_error(format!(
                "--local needs a git repository, and {} is not inside one",
                cwd.display()
            ))
        })?;
        (repo::peers_dir(&root), "local")
    } else {
        (local::dir(home), "global")
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = format!("{}.json", peer.slug);
    // Same slug, different key: never replace another person's file.
    if dir.join(&name).is_file() {
        return Err(user_error(format!(
            "{} already exists (a different key); remove it first with `owl contact remove`",
            dir.join(&name).display()
        )));
    }
    let mut bytes = serde_json::to_vec_pretty(&peer.json)?;
    bytes.push(b'\n');
    write_atomic(&dir, &name, &bytes)?;
    println!("added {} {} ({scope})", peer.name, peer.fingerprint);
    Ok(())
}

/// `-` = stdin, a leading `{` = inline JSON, anything else = a file path.
fn read_source(source: &str) -> anyhow::Result<String> {
    if source == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading the peer file from stdin")?;
        return Ok(s);
    }
    if source.trim_start().starts_with('{') {
        return Ok(source.to_string());
    }
    std::fs::read_to_string(source)
        .map_err(|e| user_error(format!("reading peer file {source}: {e}")))
}

#[derive(Debug)]
pub struct Peer {
    pub name: String,
    pub fingerprint: String,
    pub slug: String,
    /// Exactly `name`, `pubkey`, and `emails` / `endpoints` when given.
    pub json: Value,
}

/// Parse and check a peer file: `name` (non-empty string) and `pubkey` (parsable) are
/// required, `emails` / `endpoints` optional arrays of strings; every other field is dropped.
pub fn validate(text: &str) -> anyhow::Result<Peer> {
    let v: Value = serde_json::from_str(text)
        .map_err(|e| user_error(format!("peer file is not valid JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| user_error("peer file must be a JSON object"))?;
    let name = match obj.get("name") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(Value::String(_)) => return Err(user_error("peer file field `name` is empty")),
        Some(_) => return Err(user_error("peer file field `name` must be a string")),
        None => return Err(user_error("peer file lacks the `name` field")),
    };
    let pubkey = match obj.get("pubkey") {
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err(user_error("peer file field `pubkey` must be a string")),
        None => return Err(user_error("peer file lacks the `pubkey` field")),
    };
    let key = identity::parse_pubkey(&pubkey)
        .map_err(|e| user_error(format!("peer file field `pubkey` is invalid: {e:#}")))?;
    let slug = slug(&name);
    if slug.is_empty() {
        return Err(user_error(
            "peer file field `name` has no letters or digits to name the file by",
        ));
    }
    let mut out = Map::new();
    out.insert("name".into(), Value::String(name.clone()));
    for field in ["emails", "endpoints"] {
        if let Some(list) = string_list(obj, field)? {
            out.insert(field.into(), Value::Array(list));
        }
    }
    out.insert("pubkey".into(), Value::String(pubkey));
    Ok(Peer {
        name,
        fingerprint: identity::fingerprint(&key),
        slug,
        json: Value::Object(out),
    })
}

/// An optional array-of-strings field; `None` when absent.
fn string_list(obj: &Map<String, Value>, field: &str) -> anyhow::Result<Option<Vec<Value>>> {
    match obj.get(field) {
        None => Ok(None),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => Ok(Some(items.clone())),
        Some(_) => Err(user_error(format!(
            "peer file field `{field}` must be an array of strings"
        ))),
    }
}

/// File stem for a contact: lowercased, every run of non-alphanumerics becomes one `-`,
/// no leading or trailing `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use owlpost::identity::{Identity, pubkey_string};

    fn pk() -> String {
        pubkey_string(&Identity::from_seed([7; 32]).verifying_key())
    }

    #[test]
    fn slug_rule_exact() {
        assert_eq!(slug("Martin"), "martin");
        assert_eq!(slug("Owen Nolan"), "owen-nolan");
        assert_eq!(slug("  Jan   Kowalski-Śmiały!! "), "jan-kowalski-mia-y");
        assert_eq!(slug("A_B.C"), "a-b-c");
        assert_eq!(slug("---x---"), "x");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug("R2D2"), "r2d2");
    }

    #[test]
    fn validate_keeps_only_the_four_fields() {
        let text = serde_json::json!({
            "policy": {"mode": "auto"}, "source": "local", "fingerprint": "owl:fake",
            "endpoints": ["h:1"], "pubkey": pk(), "emails": ["a@x"], "name": " Owen ",
        })
        .to_string();
        let p = validate(&text).unwrap();
        assert_eq!(p.name, "Owen");
        assert_eq!(p.slug, "owen");
        assert_eq!(
            p.fingerprint,
            identity::fingerprint(&Identity::from_seed([7; 32]).verifying_key())
        );
        let keys: Vec<&str> = p
            .json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["emails", "endpoints", "name", "pubkey"]);
        assert_eq!(p.json["name"], "Owen");
        assert_eq!(p.json["emails"], serde_json::json!(["a@x"]));
        assert_eq!(p.json["endpoints"], serde_json::json!(["h:1"]));
        assert_eq!(p.json["pubkey"], pk());
    }

    #[test]
    fn validate_optional_lists_absent_stay_absent() {
        let p = validate(&format!(r#"{{"name":"Owen","pubkey":"{}"}}"#, pk())).unwrap();
        let keys: Vec<&str> = p
            .json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["name", "pubkey"]);
    }

    #[test]
    fn validate_rejects_each_bad_shape_naming_the_field() {
        let cases: Vec<(String, &str)> = vec![
            ("{not json".into(), "not valid JSON"),
            ("[]".into(), "must be a JSON object"),
            (r#""x""#.into(), "must be a JSON object"),
            (
                format!(r#"{{"pubkey":"{}"}}"#, pk()),
                "lacks the `name` field",
            ),
            (
                format!(r#"{{"name":"","pubkey":"{}"}}"#, pk()),
                "`name` is empty",
            ),
            (
                format!(r#"{{"name":"  ","pubkey":"{}"}}"#, pk()),
                "`name` is empty",
            ),
            (
                format!(r#"{{"name":7,"pubkey":"{}"}}"#, pk()),
                "`name` must be a string",
            ),
            (
                format!(r#"{{"name":"!!!","pubkey":"{}"}}"#, pk()),
                "no letters or digits",
            ),
            (r#"{"name":"Owen"}"#.into(), "lacks the `pubkey` field"),
            (
                r#"{"name":"Owen","pubkey":5}"#.into(),
                "`pubkey` must be a string",
            ),
            (
                r#"{"name":"Owen","pubkey":"ed25519:AAAA"}"#.into(),
                "`pubkey` is invalid",
            ),
            (
                r#"{"name":"Owen","pubkey":"AAAA"}"#.into(),
                "`pubkey` is invalid",
            ),
            (
                format!(r#"{{"name":"Owen","pubkey":"{}","emails":"a@x"}}"#, pk()),
                "`emails` must be an array of strings",
            ),
            (
                format!(r#"{{"name":"Owen","pubkey":"{}","emails":[1]}}"#, pk()),
                "`emails` must be an array of strings",
            ),
            (
                format!(r#"{{"name":"Owen","pubkey":"{}","endpoints":{{}}}}"#, pk()),
                "`endpoints` must be an array of strings",
            ),
        ];
        for (text, needle) in cases {
            let err = validate(&text).unwrap_err();
            assert!(err.to_string().contains(needle), "{text}: {err}");
            assert_eq!(super::super::exit_code(&err), 1, "{text}");
        }
    }
}
