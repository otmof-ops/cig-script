// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Ed25519 public keys in OpenSSH's text form, and allowed-signers files:
//! the same format `ssh-keygen -Y verify` and `cigcompile verify
//! --allowed-signers` read, so one file can trust a machine for both.

use base64::Engine;

const KEY_TYPE: &str = "ssh-ed25519";
/// The namespaces a key is trusted for passes under: `cigpass-<kind>`, one
/// per kind it may issue (`namespaces="cigpass-check,cigpass-program"`).
pub const NAMESPACE_PREFIX: &str = "cigpass-";

/// `ssh-ed25519 AAAA...`: a public key as one line of text.
pub fn to_openssh(pk: &[u8; 32]) -> String {
    let mut wire = Vec::with_capacity(51);
    wire.extend_from_slice(&(KEY_TYPE.len() as u32).to_be_bytes());
    wire.extend_from_slice(KEY_TYPE.as_bytes());
    wire.extend_from_slice(&32u32.to_be_bytes());
    wire.extend_from_slice(pk);
    format!(
        "{KEY_TYPE} {}",
        base64::engine::general_purpose::STANDARD.encode(wire)
    )
}

/// The key in `ssh-ed25519 AAAA...` (the base64 part), if it is one.
pub fn from_openssh_b64(b64: &str) -> Option<[u8; 32]> {
    let wire = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let mut at = 0usize;
    let mut field = || -> Option<Vec<u8>> {
        let n = u32::from_be_bytes(wire.get(at..at + 4)?.try_into().ok()?) as usize;
        let f = wire.get(at + 4..at + 4 + n)?.to_vec();
        at += 4 + n;
        Some(f)
    };
    if field()? != KEY_TYPE.as_bytes() {
        return None;
    }
    let key = field()?;
    if at != wire.len() {
        return None;
    }
    key.try_into().ok()
}

/// One trusted key from an allowed-signers file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signer {
    pub principals: String,
    pub key: [u8; 32],
    /// The kinds of pass this key may issue here.
    pub kinds: Vec<String>,
}

/// Read an allowed-signers file: `principals [options] ssh-ed25519 AAAA...
/// [comment]` per line. Other key types are skipped (passes are Ed25519).
/// A key is trusted for the kinds its `namespaces=` option names as
/// `cigpass-<kind>`; a line with no such namespace allows no pass at all,
/// and a `cert-authority` line is skipped (passes are not certificates).
pub fn parse_allowed_signers(text: &str) -> Vec<Signer> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words = split_respecting_quotes(line);
        let Some(at) = words.iter().position(|w| w == KEY_TYPE) else {
            continue;
        };
        if at == 0 {
            continue;
        }
        let Some(key) = words.get(at + 1).and_then(|b| from_openssh_b64(b)) else {
            continue;
        };
        let options: Vec<&String> = words[1..at].iter().collect();
        let mut kinds: Vec<String> = Vec::new();
        let mut ok = true;
        for o in options {
            for opt in split_options(o) {
                let opt_l = opt.to_ascii_lowercase();
                if opt_l == "cert-authority" {
                    ok = false;
                }
                if let Some(ns) = opt_l.strip_prefix("namespaces=") {
                    for n in ns.trim_matches('"').split(',') {
                        if let Some(k) = n.trim().strip_prefix(NAMESPACE_PREFIX) {
                            if crate::is_kind(k) {
                                kinds.push(k.to_string());
                            }
                        }
                    }
                }
            }
        }
        if ok && !kinds.is_empty() {
            out.push(Signer {
                principals: words[0].clone(),
                key,
                kinds,
            });
        }
    }
    out
}

fn split_respecting_quotes(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                cur.push(ch);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Split an options word on commas outside quotes:
/// `namespaces="a,b",valid-after="..."`.
fn split_options(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in word.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                cur.push(ch);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A line for an allowed-signers file trusting `pk` for these kinds.
pub fn allowed_signers_line(principal: &str, pk: &[u8; 32], kinds: &[&str]) -> String {
    let ns: Vec<String> = kinds
        .iter()
        .map(|k| format!("{NAMESPACE_PREFIX}{k}"))
        .collect();
    format!(
        "{principal} namespaces=\"{}\" {}",
        ns.join(","),
        to_openssh(pk)
    )
}
