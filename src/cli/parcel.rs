// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig parcel <run>` and `cig unparcel <file>`: a run's undo, carried to
//! another machine.

use super::{exit, Ctx};
use crate::burn::journal::Journal;
use crate::burn::parcel;
use crate::burn::runs;
use std::path::{Path, PathBuf};

pub fn parcel(ctx: &Ctx, id: &str, out: Option<PathBuf>, with_key: bool) -> i32 {
    let rec = match runs::find(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("{} no run matches `{id}`", ctx.red("error[E803 usage]:"));
            eprintln!("  = hint: `cig runs` lists them; give more of the id if it is ambiguous");
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E803 usage]:"));
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
    };
    let journal = match Journal::load(&rec.dir()) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "{} cannot load the journal for {}: {e}",
                ctx.red("error[E703 burn]:"),
                rec.id
            );
            eprintln!("  = explain: cig explain E703");
            return exit::SCRIPT_ERROR;
        }
    };
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{}.{}", rec.id, parcel::EXT)));
    let opts = parcel::WriteOpts { with_key };
    match parcel::write_with(&rec, &journal, &out, &opts) {
        Ok(w) => {
            // The parcel's pass, beside it: who made it, and that nothing in
            // it changed (cig-script#186). No key here: no pass, and the
            // parcel is as it always was.
            let pass_path = PathBuf::from(format!("{}.pass", w.path.display()));
            match crate::pass::issue_parcel_pass(&rec.id, &w.manifest_sha256) {
                Ok(p) => {
                    let mut o = std::fs::OpenOptions::new();
                    o.write(true).create(true).truncate(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                    }
                    let wrote = o.open(&pass_path).and_then(|mut f| {
                        use std::io::Write;
                        f.write_all(p.token().as_bytes())?;
                        f.write_all(b"\n")
                    });
                    if let Err(e) = wrote {
                        eprintln!(
                            "{} the parcel's pass was not written ({e}); the parcel is good, but another machine cannot tell who made it",
                            ctx.yellow("note:")
                        );
                    }
                }
                Err(e) => eprintln!("{}", ctx.dim(&format!("no pass beside the parcel ({e})"))),
            }
            if ctx.json {
                // --json strips the raw sealing key from the embedded
                // manifest and prints its fingerprint instead
                // (SEC-004 M1); the parcel on disk still carries the
                // real key when --with-key was asked for.
                outln!(
                    "{}",
                    serde_json::to_string(&parcel::written_for_display(&w)).unwrap_or_default()
                );
            } else {
                outln!("{}", w.path.display());
                let roots: Vec<String> = w
                    .manifest
                    .roots
                    .iter()
                    .map(|r| format!("{}={}", r.name, r.path.display()))
                    .collect();
                if w.manifest.sealing_key.is_some() {
                    eprintln!(
                        "{} this parcel carries the tin's seal key: anyone with the file can open its sealed objects. Guard it.",
                        ctx.red("warning:")
                    );
                }
                eprintln!(
                    "{} run {} parcelled: {} file{}, {} bytes{}{}; on the other machine: cig unparcel {} {}",
                    ctx.green("parcel:"),
                    rec.id,
                    w.files,
                    if w.files == 1 { "" } else { "s" },
                    w.bytes,
                    if w.manifest.sealed.objects > 0 {
                        format!(
                            ", {} sealed (the key {} must be there too)",
                            w.manifest.sealed.objects,
                            w.manifest.sealed.key.as_deref().unwrap_or("?")
                        )
                    } else {
                        String::new()
                    },
                    if w.manifest.git_blobs > 0 {
                        format!(", {} blob{} git held carried whole", w.manifest.git_blobs, if w.manifest.git_blobs == 1 { "" } else { "s" })
                    } else {
                        String::new()
                    },
                    w.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    roots
                        .iter()
                        .map(|r| format!("--root {r}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            exit::OK
        }
        Err(e) => {
            eprintln!(
                "{} cannot write the parcel of {}: {e}",
                ctx.red("error[E762 burn]:"),
                rec.id
            );
            eprintln!("  = explain: cig explain E762");
            exit::SCRIPT_ERROR
        }
    }
}

/// `--root name=path`, parsed.
pub fn parse_root(s: &str) -> Result<(String, PathBuf), String> {
    match s.split_once('=') {
        Some((name, path)) if !name.trim().is_empty() && !path.trim().is_empty() => {
            Ok((name.trim().to_string(), PathBuf::from(path.trim())))
        }
        _ => Err(format!("--root wants name=path, got `{s}`")),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn unparcel(
    ctx: &Ctx,
    file: &Path,
    roots: Vec<String>,
    cwd: Option<PathBuf>,
    allow_sealed_missing: bool,
    accept_key: bool,
    show: bool,
    require_pass: bool,
) -> i32 {
    // The parcel's pass, when one is beside it: who made the parcel, and
    // that nothing in it changed since. `--require-pass` refuses a parcel
    // without one that holds here.
    let pass_path = PathBuf::from(format!("{}.pass", file.display()));
    let checked = if pass_path.is_file() {
        let outcome = crate::pass::read_pass(&pass_path.to_string_lossy()).and_then(|p| {
            let digest = parcel::manifest_sha256(file).map_err(|e| e.to_string())?;
            crate::pass::check_parcel_pass(&p, None, &digest)
        });
        Some(outcome)
    } else {
        None
    };
    match &checked {
        Some(Ok(signer)) => eprintln!(
            "{} made by {signer}, and unchanged since (its pass holds)",
            ctx.green("unparcel:")
        ),
        Some(Err(why)) => {
            if require_pass {
                eprintln!(
                    "{} the parcel's pass does not hold here: {why}",
                    ctx.red("error[E730 burn]:")
                );
                eprintln!("  = hint: trust its maker for parcels (cig pass key --kinds parcel on that machine), or import without --require-pass");
                return exit::SCRIPT_ERROR;
            }
            eprintln!(
                "{} the parcel's pass does not hold here: {why}",
                ctx.yellow("note:")
            );
        }
        None if require_pass => {
            eprintln!(
                "{} {} has no pass beside it ({} is not there)",
                ctx.red("error[E730 burn]:"),
                file.display(),
                pass_path.display()
            );
            return exit::SCRIPT_ERROR;
        }
        None => {}
    }
    if show {
        return match parcel::read_manifest(file) {
            Ok(m) => {
                if ctx.json {
                    // --json strips the raw sealing key and prints its
                    // fingerprint instead (SEC-004 M1).
                    outln!(
                        "{}",
                        serde_json::to_string(&parcel::manifest_for_display(&m))
                            .unwrap_or_default()
                    );
                } else {
                    outln!("run       {}", m.run);
                    outln!("written   {} by cig {}", m.created, m.cig);
                    outln!(
                        "from      {}{} (working directory {})",
                        if m.origin.host.is_empty() {
                            String::new()
                        } else {
                            format!("{}:", m.origin.host)
                        },
                        m.origin.state_dir.display(),
                        m.origin.cwd
                    );
                    for r in &m.roots {
                        outln!(
                            "root      {} = {}  (map it: --root {}=/path/here)",
                            r.name,
                            r.path.display(),
                            r.name
                        );
                    }
                    outln!(
                        "files     {}{}{}",
                        m.files.len(),
                        if m.sealed.objects > 0 {
                            format!(
                                ", {} sealed under key {}",
                                m.sealed.objects,
                                m.sealed.key.as_deref().unwrap_or("?")
                            )
                        } else {
                            String::new()
                        },
                        if m.git_blobs > 0 {
                            format!(", {} git blobs carried whole", m.git_blobs)
                        } else {
                            String::new()
                        }
                    );
                    if m.sealing_key.is_some() {
                        outln!(
                            "key       inside the parcel (--with-key): opens the {} sealed object{} without a key on this machine; parcel is a secret",
                            m.sealed.objects,
                            if m.sealed.objects == 1 { "" } else { "s" }
                        );
                    }
                }
                exit::OK
            }
            Err(e) => {
                eprintln!("{} {}: {e}", ctx.red("error[E762 burn]:"), file.display());
                eprintln!("  = explain: cig explain E762");
                exit::SCRIPT_ERROR
            }
        };
    }
    let mut mapping = parcel::Mapping {
        roots: Vec::new(),
        cwd,
        allow_sealed_missing,
        accept_key,
    };
    for r in roots {
        match parse_root(&r) {
            Ok(pair) => mapping.roots.push(pair),
            Err(e) => {
                eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
                return exit::USAGE;
            }
        }
    }
    match parcel::import(file, &mapping) {
        Ok(done) => {
            if ctx.json {
                outln!("{}", serde_json::to_string(&done).unwrap_or_default());
            } else {
                outln!("{}", done.id);
                eprintln!(
                    "{} run {} imported from {} ({} file{} checked); cig unburn {} works here, against this machine's files{}",
                    ctx.green("unparcel:"),
                    done.id,
                    done.origin,
                    done.files,
                    if done.files == 1 { "" } else { "s" },
                    done.id,
                    if done.sealed_unopenable {
                        "; its sealed objects do not open with this machine's key, and a restore from one will refuse"
                    } else {
                        ""
                    }
                );
                if let Some(fp) = &done.accepted_key_fingerprint {
                    // The fingerprint is computed from the CARRIED key
                    // bytes, not the manifest's own claim, so the
                    // operator can check it against what the sender
                    // expected (SEC-004 M2).
                    eprintln!(
                        "{} accepted carried sealing key (fingerprint {fp})",
                        ctx.green("unparcel:")
                    );
                }
            }
            exit::OK
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E762 burn]:"));
            eprintln!("  = hint: nothing was imported; cig unparcel --show {} lists what the parcel holds and the roots to map", file.display());
            eprintln!("  = explain: cig explain E762");
            exit::SCRIPT_ERROR
        }
    }
}
