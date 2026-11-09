// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig pass`: the passes this machine holds, what each depends on and
//! whether it still holds; narrowing, revoking, sharing (cig-script#186).
//!
//! A pass is read from a token, a file holding one, `-` (stdin) or an id
//! (or the start of one) of a pass in the store. A token is a bearer
//! credential: give it as a file or on stdin, not as an argument others on
//! the machine can see.

use super::{exit, Ctx};
use crate::pass::{self, CigCtx, Keys};
use cig_pass::{Form, Input, Pass};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum PassCommand {
    /// The passes kept here, newest first.
    List {
        /// Only this kind.
        #[arg(long)]
        kind: Option<String>,
    },
    /// What a pass is about, what it depends on, its caveats, and whether it holds now.
    Show {
        /// A pass: a file, `-` for stdin, or an id from cig pass list.
        pass: String,
    },
    /// Does a pass hold now? Exit 0 when it does, 1 when it does not.
    Verify {
        /// A pass: a file, `-` for stdin, or an id from cig pass list.
        pass: String,
        /// Trust these signers (OpenSSH allowed-signers format) instead of the configured file.
        #[arg(long, value_name = "FILE")]
        allowed_signers: Option<PathBuf>,
        /// The action works in this folder (for `within`); repeat for more.
        #[arg(long = "root", value_name = "DIR")]
        roots: Vec<PathBuf>,
        /// The action runs in a box (for `boxed`).
        #[arg(long)]
        boxed: bool,
        /// The action's network: `none`, or hosts separated by commas (for `network`).
        #[arg(long, value_name = "POLICY")]
        network: Option<String>,
        /// The action's arguments, one per flag (for `args`).
        #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
        args: Vec<String>,
        /// The agent the action runs under (for `agent`).
        #[arg(long, value_name = "NAME")]
        agent: Option<String>,
    },
    /// Add caveats to a pass (each only narrows it) and print the new token.
    Narrow {
        /// A pass: a file, `-` for stdin, or an id from cig pass list.
        pass: String,
        /// Caveats, one per argument: "within ~/work/site" "for 2h" once.
        #[arg(required = true)]
        caveats: Vec<String>,
        /// Write the token to this file (0600) instead of stdout.
        #[arg(short = 'o', long = "out", value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Revoke a pass (and everything narrowed from it), or every pass of a kind.
    Revoke {
        /// A pass: a file, `-` for stdin, or an id (an id of a pass not kept here is revoked as given).
        pass: Option<String>,
        /// Move this kind to its next epoch: every pass of the kind stops holding.
        #[arg(long, conflicts_with = "pass")]
        kind: Option<String>,
    },
    /// A pass re-issued as a shareable one, signed by this machine: what another machine or a CI job can verify.
    Export {
        /// A pass: a file, `-` for stdin, or an id from cig pass list.
        pass: String,
        /// Write it to this file (0600) instead of stdout.
        #[arg(short = 'o', long = "out", value_name = "FILE")]
        out: Option<PathBuf>,
        /// Leave it open for its holder to narrow further (sealed by default).
        #[arg(long)]
        unsealed: bool,
        /// How long it holds (default 7d).
        #[arg(long = "for", value_name = "SPAN", default_value = "7d")]
        for_: String,
    },
    /// Keep a shareable pass from another machine, once it verifies here.
    Import {
        /// A file holding the pass, or `-` for stdin.
        file: String,
        /// Trust these signers instead of the configured file.
        #[arg(long, value_name = "FILE")]
        allowed_signers: Option<PathBuf>,
    },
    /// The ledger's pass lines: spends, revocations, epochs.
    Log,
    /// The store: passes kept, by kind, and its size.
    Stats,
    /// This machine's public key for shareable passes, as an allowed-signers line.
    Key {
        /// The kinds another machine may accept from this one.
        #[arg(long, value_delimiter = ',', default_value = "check,program")]
        kinds: Vec<String>,
    },
    /// Compact the store and apply its budget now.
    Gc,
}

fn err(ctx: &Ctx, code: &str, msg: &str) {
    eprintln!("{} {msg}", ctx.red(&format!("error[{code} burn]:")));
}

fn usage_err(ctx: &Ctx, msg: &str) {
    eprintln!("{} {msg}", ctx.red("error[E809 usage]:"));
}

fn open_or_report(ctx: &Ctx) -> Option<(Keys, cig_pass::marks::Marks)> {
    let keys = match Keys::load() {
        Ok(k) => k,
        Err(e) => {
            err(
                ctx,
                "E721",
                &format!("the tin's key is not available ({e}); no pass can be checked"),
            );
            return None;
        }
    };
    match pass::state(&keys) {
        Ok(m) => Some((keys, m)),
        Err(e) => {
            let code = if e.starts_with(pass::STATE_E739) {
                "E739"
            } else {
                "E721"
            };
            err(ctx, code, &e);
            None
        }
    }
}

fn read_or_report(ctx: &Ctx, arg: &str) -> Option<Pass> {
    match pass::read_pass(arg) {
        Ok(p) => Some(p),
        Err(e) => {
            err(ctx, "E736", &e);
            None
        }
    }
}

fn input_line(i: &Input) -> String {
    match i {
        Input::File { path, racy, .. } => format!(
            "file     {}{}",
            cig_pass::shown(path),
            if *racy {
                "  (changed as it was read: always hashed)"
            } else {
                ""
            }
        ),
        Input::Absent { path } => format!("absent   {}", cig_pass::shown(path)),
        Input::Dir { path, .. } => format!("folder   {}", cig_pass::shown(path)),
        Input::Present { path, kind } => {
            format!("present  {} ({})", cig_pass::shown(path), kind.word())
        }
        Input::Env { name, .. } if name.as_slice() == b":cwd" => {
            "env      the working folder".to_string()
        }
        Input::Env { name, value } => format!(
            "env      ${}{}",
            cig_pass::clean(&String::from_utf8_lossy(name)),
            if value.is_none() { " (unset)" } else { "" }
        ),
        Input::Tool {
            name,
            version,
            build,
        } => format!(
            "tool     {} {} ({})",
            cig_pass::clean(name),
            cig_pass::clean(version),
            cig_pass::clean(build)
        ),
        Input::Rules { name, .. } => format!("rules    {}", cig_pass::clean(name)),
        Input::Pass { id } => format!("pass     {}", cig_pass::hex(id)),
        Input::Clock { not_after_ms } => format!("until    {}", cig_pass::iso(*not_after_ms)),
    }
}

/// One input for `--json`: its kind and what it names (never a digest).
fn input_json(i: &Input) -> serde_json::Value {
    let (kind, name) = match i {
        Input::File { path, .. } => ("file", cig_pass::shown(path)),
        Input::Absent { path } => ("absent", cig_pass::shown(path)),
        Input::Dir { path, .. } => ("dir", cig_pass::shown(path)),
        Input::Present { path, .. } => ("present", cig_pass::shown(path)),
        Input::Env { name, .. } => ("env", cig_pass::clean(&String::from_utf8_lossy(name))),
        Input::Tool {
            name,
            version,
            build,
        } => (
            "tool",
            cig_pass::clean(&format!("{name} {version} {build}")),
        ),
        Input::Rules { name, .. } => ("rules", cig_pass::clean(name)),
        Input::Pass { id } => ("pass", cig_pass::hex(id)),
        Input::Clock { not_after_ms } => ("clock", cig_pass::iso(*not_after_ms)),
    };
    serde_json::json!({"kind": kind, "name": name})
}

fn form_word(p: &Pass) -> &'static str {
    match p.form {
        Form::Local => "local",
        Form::Shareable => "shareable",
    }
}

fn write_token(ctx: &Ctx, token: &str, out: &Option<PathBuf>) -> i32 {
    match out {
        None => {
            outln!("{token}");
            exit::OK
        }
        Some(path) => {
            // Never through a link, and 0600 whatever the file was before:
            // a token is a bearer credential.
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            match o.open(path).and_then(|mut f| {
                use std::io::Write;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
                f.write_all(token.as_bytes())?;
                f.write_all(b"\n")
            }) {
                Ok(()) => {
                    eprintln!("wrote {}", path.display());
                    exit::OK
                }
                Err(e) => {
                    usage_err(ctx, &format!("cannot write {}: {e}", path.display()));
                    exit::USAGE
                }
            }
        }
    }
}

pub fn pass_cmd(ctx: &Ctx, cmd: Option<PassCommand>) -> i32 {
    match cmd.unwrap_or(PassCommand::List { kind: None }) {
        PassCommand::List { kind } => list(ctx, kind.as_deref()),
        PassCommand::Show { pass: arg } => show(ctx, &arg),
        PassCommand::Verify {
            pass: arg,
            allowed_signers,
            roots,
            boxed,
            network,
            args,
            agent,
        } => {
            let mut action = cig_pass::Action {
                boxed,
                agent,
                ..Default::default()
            };
            for r in roots {
                let abs = std::fs::canonicalize(&r).unwrap_or(r);
                if let Some(b) = cig_pass::path_bytes(&abs) {
                    action.roots.push(b);
                }
            }
            if !args.is_empty() {
                action.argv = Some(args);
            }
            action.network = match network.as_deref() {
                None => None,
                Some("none") => Some(cig_pass::Network::None),
                Some(hosts) => Some(cig_pass::Network::Hosts(
                    hosts
                        .split(',')
                        .map(|h| h.trim().to_ascii_lowercase())
                        .filter(|h| !h.is_empty())
                        .collect(),
                )),
            };
            verify(ctx, &arg, allowed_signers, action)
        }
        PassCommand::Narrow {
            pass: arg,
            caveats,
            out,
        } => narrow(ctx, &arg, &caveats, &out),
        PassCommand::Revoke { pass: arg, kind } => revoke(ctx, arg.as_deref(), kind.as_deref()),
        PassCommand::Export {
            pass: arg,
            out,
            unsealed,
            for_,
        } => export(ctx, &arg, &out, unsealed, &for_),
        PassCommand::Import {
            file,
            allowed_signers,
        } => import(ctx, &file, allowed_signers),
        PassCommand::Log => log(ctx),
        PassCommand::Stats => stats(ctx),
        PassCommand::Key { kinds } => key(ctx, &kinds),
        PassCommand::Gc => gc(ctx),
    }
}

fn list(ctx: &Ctx, kind: Option<&str>) -> i32 {
    let s = match pass::store() {
        Ok(s) => s,
        Err(e) => {
            err(ctx, "E739", &e);
            return exit::SCRIPT_ERROR;
        }
    };
    let mut rows: Vec<Pass> = match s.list(kind) {
        Ok(v) => v
            .into_iter()
            .filter_map(|(_, b)| Pass::decode(&b).ok())
            .collect(),
        Err(e) => {
            err(ctx, "E739", &format!("the pass store: {e}"));
            return exit::SCRIPT_ERROR;
        }
    };
    rows.sort_by_key(|p| std::cmp::Reverse(p.issued_ms));
    if ctx.json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": cig_pass::hex(&p.id()), "root": cig_pass::hex(&p.root_id()),
                    "kind": p.kind, "form": form_word(p), "issued": cig_pass::iso(p.issued_ms),
                    "inputs": p.inputs.len(),
                    "caveats": p.caveats.iter().map(cig_pass::caveat_text).collect::<Vec<_>>(),
                })
            })
            .collect();
        outln!("{}", serde_json::Value::Array(v));
        return exit::OK;
    }
    if rows.is_empty() {
        outln!(
            "no passes kept here{}",
            kind.map(|k| format!(" of kind {k}")).unwrap_or_default()
        );
        return exit::OK;
    }
    for p in &rows {
        outln!(
            "{}  {:<9} {:<9} {}  {} input(s){}",
            &cig_pass::hex(&p.id())[..16],
            p.kind,
            form_word(p),
            cig_pass::iso(p.issued_ms),
            p.inputs.len(),
            if p.caveats.is_empty() {
                String::new()
            } else {
                format!(
                    "  [{}]",
                    p.caveats
                        .iter()
                        .map(|c| cig_pass::clean(&cig_pass::caveat_text(c)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }
    exit::OK
}

fn check_now(
    keys: &Keys,
    marks: &cig_pass::marks::Marks,
    p: &Pass,
    signers: Option<PathBuf>,
    action: cig_pass::Action,
) -> Result<cig_pass::Verified, cig_pass::Why> {
    let st = pass::store().ok();
    let mut c = CigCtx::new(keys, marks, st.as_ref());
    if let Some(f) = signers {
        c.signers = pass::read_signers(&f);
    }
    c.action = action;
    cig_pass::verify(p, &c)
}

fn show(ctx: &Ctx, arg: &str) -> i32 {
    let Some(p) = read_or_report(ctx, arg) else {
        return exit::USAGE;
    };
    let Some((keys, marks)) = open_or_report(ctx) else {
        return exit::SCRIPT_ERROR;
    };
    let now = check_now(&keys, &marks, &p, None, cig_pass::Action::default());
    if ctx.json {
        let inputs: Vec<serde_json::Value> = p.inputs.iter().map(input_json).collect();
        outln!(
            "{}",
            serde_json::json!({
                "id": cig_pass::hex(&p.id()), "root": cig_pass::hex(&p.root_id()),
                "prefix_ids": p.prefix_ids().iter().map(|i| cig_pass::hex(i)).collect::<Vec<_>>(),
                "kind": p.kind, "form": form_word(&p), "issued": cig_pass::iso(p.issued_ms),
                "epoch": p.epoch, "declared_reads": p.flags & cig_pass::FLAG_DECLARED_READS != 0,
                "inputs": inputs,
                "caveats": p.caveats.iter().map(cig_pass::caveat_text).collect::<Vec<_>>(),
                "valid": now.is_ok(), "why": now.as_ref().err().map(ToString::to_string),
            })
        );
        return exit::OK;
    }
    outln!("pass {}", cig_pass::hex(&p.id()));
    outln!("  root     {}", cig_pass::hex(&p.root_id()));
    outln!(
        "  kind     {} ({}, epoch {})",
        p.kind,
        form_word(&p),
        p.epoch
    );
    outln!("  issued   {}", cig_pass::iso(p.issued_ms));
    if p.flags & cig_pass::FLAG_DECLARED_READS != 0 {
        outln!("  note     includes a hop's declared reads, not only what was seen");
    }
    outln!("  inputs   {}", p.inputs.len());
    for i in &p.inputs {
        outln!("    {}", input_line(i));
    }
    if !p.caveats.is_empty() {
        outln!("  caveats");
        for c in &p.caveats {
            outln!("    {}", cig_pass::clean(&cig_pass::caveat_text(c)));
        }
    }
    match now {
        Ok(_) => outln!("  holds now"),
        Err(w) => outln!("  does not hold: {w}"),
    }
    exit::OK
}

fn verify(ctx: &Ctx, arg: &str, signers: Option<PathBuf>, action: cig_pass::Action) -> i32 {
    let Some(p) = read_or_report(ctx, arg) else {
        return exit::USAGE;
    };
    let Some((keys, marks)) = open_or_report(ctx) else {
        return exit::SCRIPT_ERROR;
    };
    let r = check_now(&keys, &marks, &p, signers, action);
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({"id": cig_pass::hex(&p.id()), "valid": r.is_ok(), "why": r.as_ref().err().map(ToString::to_string)})
        );
    } else {
        match &r {
            Ok(_) => outln!("ok: {} holds", &cig_pass::hex(&p.id())[..16]),
            Err(w) => {
                let code = match w {
                    cig_pass::Why::UnknownSigner => "E738",
                    cig_pass::Why::Discharge(_) => "E737",
                    cig_pass::Why::Spent { .. } => "E735",
                    _ => "E730",
                };
                err(
                    ctx,
                    code,
                    &format!("{} does not hold: {w}", &cig_pass::hex(&p.id())[..16]),
                );
            }
        }
    }
    if r.is_ok() {
        exit::OK
    } else {
        exit::SCRIPT_ERROR
    }
}

fn narrow(ctx: &Ctx, arg: &str, caveats: &[String], out: &Option<PathBuf>) -> i32 {
    let Some(mut p) = read_or_report(ctx, arg) else {
        return exit::USAGE;
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let cwd = std::env::current_dir().unwrap_or_default();
    for t in caveats {
        let c = match cig_pass::parse_caveat(t, home.as_deref(), &cwd, cig_pass::wall_ms()) {
            Ok(c) => c,
            Err(e) => {
                usage_err(ctx, &format!("`{t}`: {e}"));
                return exit::USAGE;
            }
        };
        let c = match c {
            cig_pass::Caveat::Within(path) => {
                let resolved = cig_pass::bytes_path(&path)
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    .and_then(|p| cig_pass::path_bytes(&p))
                    .unwrap_or(path);
                cig_pass::Caveat::Within(resolved)
            }
            other => other,
        };
        p = match cig_pass::attenuate(&p, c) {
            Ok(n) => n,
            Err(e) => {
                err(ctx, "E730", &e);
                return exit::SCRIPT_ERROR;
            }
        };
    }
    write_token(ctx, &p.token(), out)
}

fn revoke(ctx: &Ctx, arg: Option<&str>, kind: Option<&str>) -> i32 {
    if let Some(k) = kind {
        return match pass::revoke_kind(k) {
            Ok(e) => {
                outln!("every {k} pass issued before now stops holding (epoch {e})");
                exit::OK
            }
            Err(e) => {
                usage_err(ctx, &e);
                exit::USAGE
            }
        };
    }
    let Some(arg) = arg else {
        usage_err(ctx, "name a pass to revoke, or --kind <kind>");
        return exit::USAGE;
    };
    let id = match pass::read_pass(arg) {
        Ok(p) => p.id(),
        Err(_) => match cig_pass::from_hex32(arg) {
            Some(id) => id,
            None => {
                err(
                    ctx,
                    "E736",
                    &format!("`{arg}` is neither a pass nor a full 64-digit id"),
                );
                return exit::USAGE;
            }
        },
    };
    match pass::revoke_id(&id) {
        Ok(()) => {
            outln!(
                "revoked {} and every pass narrowed from it",
                &cig_pass::hex(&id)[..16]
            );
            exit::OK
        }
        Err(e) => {
            err(ctx, "E721", &e);
            exit::SCRIPT_ERROR
        }
    }
}

fn export(ctx: &Ctx, arg: &str, out: &Option<PathBuf>, unsealed: bool, for_: &str) -> i32 {
    let Some(p) = read_or_report(ctx, arg) else {
        return exit::USAGE;
    };
    let Some((keys, marks)) = open_or_report(ctx) else {
        return exit::SCRIPT_ERROR;
    };
    if let Err(w) = check_now(&keys, &marks, &p, None, cig_pass::Action::default()) {
        err(
            ctx,
            "E730",
            &format!("only a pass that holds here can be exported: {w}"),
        );
        return exit::SCRIPT_ERROR;
    }
    let span = match cig_pass::parse_span(for_) {
        Ok(s) => s,
        Err(e) => {
            usage_err(ctx, &e);
            return exit::USAGE;
        }
    };
    let sign = keys.signing_seed();
    let shared = match cig_pass::reissue_shareable(&p, &sign, &keys.hash_key()) {
        Ok(s) => s,
        Err(e) => {
            err(ctx, "E730", &e.to_string());
            return exit::SCRIPT_ERROR;
        }
    };
    let shared = match cig_pass::attenuate(
        &shared,
        cig_pass::Caveat::Until(cig_pass::wall_ms().saturating_add(span)),
    ) {
        Ok(s) => s,
        Err(e) => {
            err(ctx, "E730", &e);
            return exit::SCRIPT_ERROR;
        }
    };
    let shared = if unsealed {
        eprintln!("unsealed: anyone holding it can narrow it further; it is still a bearer token");
        shared
    } else {
        match cig_pass::seal(&shared) {
            Ok(s) => s,
            Err(e) => {
                err(ctx, "E730", &e);
                return exit::SCRIPT_ERROR;
            }
        }
    };
    eprintln!(
        "exported as {} (revoking {} here does not revoke it: revoke that id too)",
        &cig_pass::hex(&shared.id())[..16],
        &cig_pass::hex(&p.id())[..16]
    );
    write_token(ctx, &shared.token(), out)
}

fn import(ctx: &Ctx, file: &str, signers: Option<PathBuf>) -> i32 {
    let Some(p) = read_or_report(ctx, file) else {
        return exit::USAGE;
    };
    let Some((keys, marks)) = open_or_report(ctx) else {
        return exit::SCRIPT_ERROR;
    };
    if p.form != Form::Shareable {
        err(
            ctx,
            "E730",
            "only a shareable pass is imported; a local one holds only on the machine that made it",
        );
        return exit::SCRIPT_ERROR;
    }
    match check_now(&keys, &marks, &p, signers, cig_pass::Action::default()) {
        Ok(_) => match pass::keep(&p) {
            Ok(()) => {
                outln!("kept {} ({} pass)", &cig_pass::hex(&p.id())[..16], p.kind);
                exit::OK
            }
            Err(e) => {
                err(ctx, "E739", &e);
                exit::SCRIPT_ERROR
            }
        },
        Err(w) => {
            let code = if matches!(w, cig_pass::Why::UnknownSigner) {
                "E738"
            } else {
                "E730"
            };
            err(ctx, code, &format!("not kept: {w}"));
            exit::SCRIPT_ERROR
        }
    }
}

fn log(ctx: &Ctx) -> i32 {
    match pass::log() {
        Ok(lines) => {
            if ctx.json {
                outln!("{}", serde_json::to_string(&lines).unwrap_or_default());
                return exit::OK;
            }
            if lines.is_empty() {
                outln!("no pass has been spent, revoked or moved to a new epoch here");
            }
            for l in lines {
                let what = match l.kind.as_str() {
                    "pass-spend" => format!("spent {}", &l.head[..16.min(l.head.len())]),
                    "pass-revoke" => format!(
                        "revoked {} and what was narrowed from it",
                        &l.head[..16.min(l.head.len())]
                    ),
                    "pass-epoch" => format!("every {} pass before this stops holding", l.status),
                    _ => String::new(),
                };
                outln!("{}  #{:<5} {:<12} {what}", l.t, l.seq, l.kind);
            }
            exit::OK
        }
        Err(e) => {
            err(ctx, "E721", &e);
            exit::SCRIPT_ERROR
        }
    }
}

fn stats(ctx: &Ctx) -> i32 {
    let s = match pass::store() {
        Ok(s) => s,
        Err(e) => {
            err(ctx, "E739", &e);
            return exit::SCRIPT_ERROR;
        }
    };
    let all = s.list(None).unwrap_or_default();
    let mut by: std::collections::BTreeMap<String, (usize, u64)> = Default::default();
    for (m, b) in &all {
        let e = by.entry(m.kind.clone()).or_default();
        e.0 += 1;
        e.1 += b.len() as u64;
    }
    let disk = s.disk_bytes().unwrap_or(0);
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({"passes": all.len(), "disk_bytes": disk, "budget": pass::budget_bytes(),
                "by_kind": by.iter().map(|(k, (n, b))| serde_json::json!({"kind": k, "passes": n, "bytes": b})).collect::<Vec<_>>()})
        );
        return exit::OK;
    }
    outln!(
        "{} pass(es), {} bytes on disk (budget {})",
        all.len(),
        disk,
        pass::budget_bytes()
    );
    for (k, (n, b)) in by {
        outln!("  {k:<10} {n:>6}  {b} bytes");
    }
    exit::OK
}

fn key(ctx: &Ctx, kinds: &[String]) -> i32 {
    for k in kinds {
        match cig_pass::policy::policy(k) {
            Some(p) if p.foreign && !p.authority => {}
            _ => {
                usage_err(
                    ctx,
                    &format!("another machine never accepts a {k} pass; the kinds that travel are check, program, parcel and archive"),
                );
                return exit::USAGE;
            }
        }
    }
    let keys = match Keys::load() {
        Ok(k) => k,
        Err(e) => {
            err(
                ctx,
                "E721",
                &format!("the tin's key is not available ({e})"),
            );
            return exit::SCRIPT_ERROR;
        }
    };
    let who = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "cig".to_string());
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|_| "machine".to_string());
    let refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
    outln!(
        "{}",
        cig_pass::ssh::allowed_signers_line(&format!("{who}@{host}"), &keys.public(), &refs)
    );
    eprintln!("add that line to another machine's allowed signers (cig config pass_allowed_signers <file>); a cig tin rekey here changes it");
    exit::OK
}

fn gc(ctx: &Ctx) -> i32 {
    let keys = match Keys::load() {
        Ok(k) => k,
        Err(e) => {
            err(
                ctx,
                "E721",
                &format!("the tin's key is not available ({e})"),
            );
            return exit::SCRIPT_ERROR;
        }
    };
    // A state that no longer verifies is folded again from the ledger,
    // whose chain is checked first.
    if pass::state(&keys).is_err() {
        match pass::rebuild_state(&keys) {
            Ok(_) => eprintln!("the pass state was folded again from the ledger"),
            Err(e) => {
                err(ctx, "E739", &e);
                return exit::SCRIPT_ERROR;
            }
        }
    }
    let s = match pass::store() {
        Ok(s) => s,
        Err(e) => {
            err(ctx, "E739", &e);
            return exit::SCRIPT_ERROR;
        }
    };
    match s.compact_full(pass::budget_bytes(), &|b| Pass::decode(b).is_ok()) {
        Ok(r) => {
            let kept = s.list(None).map(|v| v.len()).unwrap_or(0);
            let disk = s.disk_bytes().unwrap_or(0);
            if ctx.json {
                outln!(
                    "{}",
                    serde_json::json!({"passes": kept, "disk_bytes": disk, "report": format!("{r:?}")})
                );
            } else {
                outln!("{kept} pass(es) kept, {disk} bytes on disk");
            }
            exit::OK
        }
        Err(e) => {
            err(ctx, "E739", &format!("the pass store: {e}"));
            exit::SCRIPT_ERROR
        }
    }
}
