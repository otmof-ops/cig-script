// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig tin`: what the tin holds, whether it is intact, and clearing what
//! no run still needs.

use super::{exit, Ctx};
use crate::burn::{rekey, seal, tin};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum TinCommand {
    /// How many objects the tin holds and how many bytes (the default).
    Du,
    /// Re-hash every object against its name, check every recipe, open every sealed object.
    Verify {
        /// Only look at each sealed object's shape instead of opening it.
        #[arg(long)]
        quick: bool,
        /// Also check the ledger's head against a value `cig tin head` printed earlier (kept elsewhere).
        #[arg(long, value_name = "HEAD")]
        head: Option<String>,
    },
    /// The ledger's head: one line to keep anywhere else, for `cig tin verify --head`.
    Head,
    /// A keyed MAC over a file, under a label of the caller's (a subkey of the tin's key): what another tool signs its own records with, and what a hop in the box cannot make.
    Mac {
        /// The file to MAC (`-` reads stdin).
        file: std::path::PathBuf,
        /// The label the subkey is derived under: lower-case letters, digits and dashes, up to 32.
        #[arg(long)]
        label: String,
        /// Check against a MAC printed earlier (exit 0 when it matches) instead of printing one.
        #[arg(long, value_name = "HEX")]
        check: Option<String>,
    },
    /// Keep a sealed copy of a file in the tin under a label of the caller's: what another tool keeps where the file it backs could be deleted (the last list it verified). The ledger records each keep.
    Keep {
        /// The file to keep.
        file: std::path::PathBuf,
        /// The caller's label: letters, digits, dashes, dots and underscores, up to 64.
        #[arg(long)]
        label: String,
        /// The name it is kept under (default: the file's name).
        #[arg(long)]
        name: Option<String>,
    },
    /// The copy kept under a label and a name, on stdout: exit 1 when nothing was kept, 4 when the ledger says one was and the tin no longer holds it as kept.
    Give {
        /// The caller's label.
        #[arg(long)]
        label: String,
        /// The name it was kept under.
        #[arg(long)]
        name: String,
    },
    /// Remove every object no run in the history still refers to.
    Gc {
        /// Also remove sealed objects the tin's key cannot open (sealed under a key that is gone).
        #[arg(long)]
        sealed_orphans: bool,
    },
    /// Back the tin's key up, or put a backed-up key in place.
    Key {
        #[command(subcommand)]
        cmd: KeyCommand,
    },
    /// Make a new tin seal key and re-seal every sealed object under it; crash-safe (a `cig tin rekey` after an interruption finishes it).
    Rekey,
}

#[derive(clap::Subcommand)]
pub enum KeyCommand {
    /// Write the key, wrapped with a passphrase, so a backup can carry it.
    Export {
        /// Where to write it (default: cig-tin-key.export).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Put a passphrase-wrapped key back as the fallback key file.
    Import {
        /// The file a previous `cig tin key export` wrote.
        file: PathBuf,
    },
}

/// The passphrase: `CIG_KEY_PASSPHRASE` when set (CI, scripts), else typed.
fn passphrase() -> Result<String, String> {
    if let Ok(p) = std::env::var("CIG_KEY_PASSPHRASE") {
        return Ok(p);
    }
    use std::io::Write;
    eprint!("passphrase (CIG_KEY_PASSPHRASE avoids the echo): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let p = line.trim_end_matches(['\n', '\r']).to_string();
    if p.is_empty() {
        return Err("an empty passphrase protects nothing".to_string());
    }
    Ok(p)
}

fn key_cmd(ctx: &Ctx, cmd: KeyCommand) -> i32 {
    match cmd {
        KeyCommand::Export { out } => {
            let dest = out.unwrap_or_else(|| PathBuf::from("cig-tin-key.export"));
            let pass = match passphrase() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{} {e}", ctx.red("error[E801 usage]:"));
                    return exit::USAGE;
                }
            };
            match seal::export_key(&dest, &pass) {
                Ok(()) => {
                    seal::mark_exported();
                    eprintln!(
                        "{} the tin's key is wrapped in {} (keep it somewhere safe; it opens your sealed objects on a restored backup)",
                        ctx.green("ok:"),
                        dest.display()
                    );
                    exit::OK
                }
                Err(e) => {
                    eprintln!("{} {e}", ctx.red("error[E703 burn]:"));
                    exit::SCRIPT_ERROR
                }
            }
        }
        KeyCommand::Import { file } => {
            let pass = match passphrase() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{} {e}", ctx.red("error[E801 usage]:"));
                    return exit::USAGE;
                }
            };
            match seal::import_key(&file, &pass) {
                Ok(()) => {
                    eprintln!(
                        "{} the key from {} is in place; your sealed objects open again",
                        ctx.green("ok:"),
                        file.display()
                    );
                    exit::OK
                }
                Err(e) => {
                    eprintln!("{} {e}", ctx.red("error[E703 burn]:"));
                    exit::SCRIPT_ERROR
                }
            }
        }
    }
}

fn rekey_cmd(ctx: &Ctx) -> i32 {
    match rekey::rekey() {
        Ok(d) => {
            if ctx.json {
                outln!("{}", serde_json::to_string(&d).unwrap_or_default());
            } else {
                let noun = if d.walked == 1 { "object" } else { "objects" };
                let when = if d.resumed {
                    "resumed and finished"
                } else {
                    "done"
                };
                eprintln!(
                    "{} {when}: {} {noun} walked, {} re-sealed, {} already under the new key. Fingerprint {} -> {}.",
                    ctx.green("rekey:"),
                    d.walked,
                    d.resealed,
                    d.already,
                    d.old_fingerprint,
                    d.new_fingerprint,
                );
                if d.resealed > 0 {
                    eprintln!("  = the old key is no longer needed; a run that was sealed before this rekey still unburns, byte-exact.");
                }
            }
            exit::OK
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E720 burn]:"));
            eprintln!("  = hint: nothing on disk was changed when the refusal came before the walk; an interrupted walk is picked up by running cig tin rekey again");
            eprintln!("  = explain: cig explain E720");
            exit::SCRIPT_ERROR
        }
    }
}

pub fn tin_cmd(ctx: &Ctx, cmd: Option<TinCommand>) -> i32 {
    match cmd.unwrap_or(TinCommand::Du) {
        TinCommand::Key { cmd } => key_cmd(ctx, cmd),
        TinCommand::Rekey => rekey_cmd(ctx),
        TinCommand::Du => {
            let u = tin::du();
            let referenced = tin::referenced().len();
            if ctx.json {
                outln!(
                    "{}",
                    serde_json::json!({"objects": u.objects, "bytes": u.stored, "stored": u.stored, "recipes": u.recipes, "logical": u.logical, "sealed": u.sealed, "sealed_bytes": u.sealed_bytes, "referenced": referenced, "dir": tin::dir()})
                );
            } else {
                outln!(
                    "{} object{} ({} bytes stored) keeping {} bytes of files{}{}; {referenced} still referred to by a run, in {}",
                    u.objects,
                    if u.objects == 1 { "" } else { "s" },
                    u.stored,
                    u.logical,
                    if u.recipes == 0 {
                        String::new()
                    } else {
                        format!(", {} of them chunked under a recipe", u.recipes)
                    },
                    if u.sealed == 0 {
                        String::new()
                    } else {
                        format!(", {} sealed ({} bytes)", u.sealed, u.sealed_bytes)
                    },
                    crate::crash::tilde(&tin::dir())
                );
            }
            exit::OK
        }
        TinCommand::Mac { file, label, check } => {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            let good_label = !label.is_empty()
                && label.len() <= 32
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !good_label {
                eprintln!(
                    "{} the label is lower-case letters, digits and dashes, up to 32 (got `{label}`)",
                    ctx.red("error[E803 usage]:")
                );
                return exit::USAGE;
            }
            let bytes = if file.as_os_str() == "-" {
                let mut v = Vec::new();
                if let Err(e) = std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut v) {
                    eprintln!("{} cannot read stdin: {e}", ctx.red("error[E803 usage]:"));
                    return exit::USAGE;
                }
                v
            } else {
                match std::fs::read(&file) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!(
                            "{} cannot read {}: {e}",
                            ctx.red("error[E803 usage]:"),
                            file.display()
                        );
                        return exit::USAGE;
                    }
                }
            };
            let master = match seal::key() {
                Ok((k, _)) => k,
                Err(e) => {
                    eprintln!(
                        "{} the tin's key is not available ({e}); nothing can be signed",
                        ctx.red("error:")
                    );
                    return exit::SCRIPT_ERROR;
                }
            };
            let sub = seal::derive(&master, format!("app:{label}").as_bytes());
            let mut m =
                Hmac::<Sha256>::new_from_slice(sub.as_bytes()).expect("HMAC takes any key length");
            m.update(&bytes);
            match check {
                None => {
                    outln!("{}", hex::encode(m.finalize().into_bytes()));
                    exit::OK
                }
                Some(want) => {
                    let ok = hex::decode(want.trim())
                        .map(|w| m.verify_slice(&w).is_ok())
                        .unwrap_or(false);
                    if ok {
                        outln!("matches");
                        exit::OK
                    } else {
                        eprintln!(
                            "{} the MAC does not match: the file or the MAC changed, or it was made under another key or label",
                            ctx.red("error:")
                        );
                        exit::SCRIPT_ERROR
                    }
                }
            }
        }
        TinCommand::Head => match crate::burn::ledger::load() {
            Ok(l) => {
                let seq = l.lines.last().map(|x| x.seq).unwrap_or(0);
                if ctx.json {
                    outln!(
                        "{}",
                        serde_json::json!({"seq": seq, "head": l.head, "broken_at": l.broken_at})
                    );
                } else {
                    outln!("{seq} {}", l.head);
                }
                if l.broken_at.is_some() {
                    exit::SCRIPT_ERROR
                } else {
                    exit::OK
                }
            }
            Err(e) => {
                eprintln!("{} {e}", ctx.red("error[E703 burn]:"));
                exit::SCRIPT_ERROR
            }
        },
        TinCommand::Verify { quick, head } => {
            let mut problems = tin::verify(quick);
            match crate::burn::ledger::load() {
                Ok(l) => {
                    if let Some(at) = l.broken_at {
                        problems.push(format!(
                            "the ledger's line {at} does not carry cig's signature (E709): it was edited, removed or reordered"
                        ));
                    }
                    if let Some(want) = head.as_deref() {
                        let want = want.split_whitespace().last().unwrap_or(want);
                        if want != l.head {
                            problems.push(format!(
                                "the ledger's head is {} but the value kept elsewhere says {want}: the ledger was cut short or swapped",
                                l.head
                            ));
                        }
                    }
                }
                Err(e) => {
                    if head.is_some() {
                        problems.push(format!("the ledger could not be read: {e}"));
                    }
                }
            }
            let objects = tin::du().objects;
            if ctx.json {
                outln!(
                    "{}",
                    serde_json::json!({"objects": objects, "problems": problems})
                );
            } else if problems.is_empty() {
                eprintln!(
                    "{} {objects} object{} hash to their names",
                    ctx.green("ok:"),
                    if objects == 1 { "" } else { "s" }
                );
            } else {
                eprintln!(
                    "{} {} problem{} in the tin:",
                    ctx.red("error[E703 burn]:"),
                    problems.len(),
                    if problems.len() == 1 { "" } else { "s" }
                );
                for p in &problems {
                    eprintln!("  - {p}");
                }
                eprintln!("  = hint: a restore from a corrupt object is refused; cig tin gc removes objects no run needs");
                eprintln!("  = explain: cig explain E703");
            }
            if problems.is_empty() {
                exit::OK
            } else {
                exit::SCRIPT_ERROR
            }
        }
        TinCommand::Keep { file, label, name } => {
            let name = name.unwrap_or_else(|| {
                file.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            match tin::keep_copy(&file, &label, &name) {
                Ok(k) => {
                    println!(
                        "kept {} under {}/{} ({} bytes, sha256 {})",
                        file.display(),
                        k.label,
                        k.name,
                        k.bytes,
                        k.sha256
                    );
                    exit::OK
                }
                Err(e) => {
                    eprintln!("{} cig tin keep: {e}", ctx.red("error[E803 usage]:"));
                    exit::USAGE
                }
            }
        }
        TinCommand::Give { label, name } => match tin::give_copy(&label, &name) {
            tin::Given::Copy(bytes, _) => {
                use std::io::Write;
                let mut out = std::io::stdout().lock();
                if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
                    return exit::SCRIPT_ERROR;
                }
                exit::OK
            }
            tin::Given::Nothing => {
                eprintln!("nothing is kept under {label}/{name}");
                exit::SCRIPT_ERROR
            }
            tin::Given::Gone(why) => {
                eprintln!("{} {why}", ctx.red("error:"));
                4
            }
        },
        TinCommand::Gc { sealed_orphans } => {
            if sealed_orphans {
                // Without the key nothing opens, and "what nothing opens" would
                // be every sealed object a run still needs.
                if let Err(e) = crate::burn::seal::key() {
                    eprintln!(
                        "{} --sealed-orphans needs the tin's key to tell a sealed object that opens from one that does not: {e}",
                        ctx.red("error[E703 burn]:")
                    );
                    eprintln!("  = hint: put the key back, or move the sealed objects away by hand to start over; nothing was removed");
                    eprintln!("  = explain: cig explain E703");
                    return exit::SCRIPT_ERROR;
                }
            }
            let (removed, bytes) = tin::gc(sealed_orphans);
            let kept = tin::du().objects;
            if ctx.json {
                outln!(
                    "{}",
                    serde_json::json!({"removed": removed, "bytes": bytes, "kept": kept})
                );
            } else {
                eprintln!(
                    "removed {removed} object{} ({bytes} bytes); {kept} kept, each referred to by a run (cig runs --prune N lets more go)",
                    if removed == 1 { "" } else { "s" }
                );
            }
            exit::OK
        }
    }
}
