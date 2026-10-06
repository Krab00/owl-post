//! `owl card [<peer>]` (§9): the A2A 1.0 agent card. Without a peer, this
//! machine's own card — from the running daemon when `daemon.addr` names one (that copy
//! lists the `owl-iroh://` interface and the relay), else built from the key and config.
//! With a peer, the card fetched from the first reachable `endpoints` entry over pinned
//! mTLS (the card is not forwardable over iroh); no reachable endpoint is exit 2 `offline`.

use std::path::Path;

use anyhow::Context;
use owlpost::client;
use owlpost::config::Config;
use owlpost::daemon;
use owlpost::server::{AppState, card_json};
use serde_json::Value;

use super::{ExitError, print_json};

pub fn run(home: &Path, peer: Option<&str>) -> anyhow::Result<()> {
    let (cfg, identity) = crate::require_identity(home)?;
    let card = match peer {
        None => own_card(home, cfg, identity)?,
        Some(query) => {
            let book = super::contact_book(home)?;
            let contact = book.resolve(query)?;
            let client = client::http_client(&identity, contact)?;
            let mut errors = Vec::new();
            let mut card = None;
            for endpoint in &contact.endpoints {
                let url = client::endpoint_url(endpoint, "/.well-known/agent-card.json");
                match client.get(&url).send().and_then(|r| r.error_for_status()) {
                    Ok(resp) => match resp.json::<Value>() {
                        Ok(v) => {
                            card = Some(v);
                            break;
                        }
                        Err(e) => errors.push(format!("{endpoint}: {e}")),
                    },
                    Err(e) => errors.push(format!("{endpoint}: {e}")),
                }
            }
            card.ok_or_else(|| {
                ExitError::error(
                    2,
                    format!(
                        "offline: no endpoint of {} reachable ({})",
                        contact.name,
                        if errors.is_empty() {
                            "no endpoints configured; the card is not served over iroh".to_string()
                        } else {
                            errors.join("; ")
                        }
                    ),
                )
            })?
        }
    };
    print_json(&card)
}

/// The daemon's card when it is running, else one built in this process (no iroh interface).
fn own_card(
    home: &Path,
    cfg: Config,
    identity: owlpost::identity::Identity,
) -> anyhow::Result<Value> {
    if let Some(addr) = daemon::local_addr(home)
        && let Ok(card) = super::doctor::fetch_card(daemon::connect_addr(addr), Some(&identity))
    {
        return Ok(card);
    }
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let state = AppState::new(home.to_path_buf(), cwd, cfg, identity, None)?;
    Ok(card_json(&state))
}
