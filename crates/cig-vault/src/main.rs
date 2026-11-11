// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig-vault`: the helper that runs as the vault's own account.
//!
//!   cig-vault serve [--root DIR] [--socket PATH] [--quota BYTES] [--grace-days N] [--detach]
//!   cig-vault gc --uid N [--root DIR] [--grace-days N] [--apply] < live.txt
//!   cig-vault ask [--socket PATH] list|has|verify <kind> [<id>]
//!   cig-vault version
//!
//! `gc` reads the live ids from standard input (`<kind> <id>` a line) and
//! prints what no live id names and is older than the grace period; with
//! `--apply` it removes them. It is run as the vault's account through a
//! sudo rule, never over the socket.

#[cfg(not(unix))]
fn main() {
    eprintln!("cig-vault runs on Linux, macOS and the BSDs");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    std::process::exit(unix::run(std::env::args().skip(1).collect()));
}

#[cfg(unix)]
mod unix {
    use cig_vault::server::{bind, serve, Options};
    use cig_vault::store::Store;
    use cig_vault::{client, Kind, DEFAULT_ROOT, DEFAULT_SOCKET};
    use std::collections::HashSet;
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    struct Args {
        root: PathBuf,
        socket: PathBuf,
        quota: u64,
        grace_days: u64,
        uid: Option<u32>,
        apply: bool,
        detach: bool,
        rest: Vec<String>,
    }

    fn parse(args: &[String]) -> Result<Args, String> {
        let mut a = Args {
            root: PathBuf::from(DEFAULT_ROOT),
            socket: PathBuf::from(DEFAULT_SOCKET),
            quota: 64 * 1024 * 1024 * 1024,
            grace_days: 7,
            uid: None,
            apply: false,
            detach: false,
            rest: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(w) = it.next() {
            let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
            match w.as_str() {
                "--root" => a.root = PathBuf::from(val("--root")?),
                "--socket" => a.socket = PathBuf::from(val("--socket")?),
                "--quota" => {
                    a.quota = val("--quota")?
                        .parse()
                        .map_err(|_| "--quota is a byte count")?
                }
                "--grace-days" => {
                    a.grace_days = val("--grace-days")?
                        .parse()
                        .map_err(|_| "--grace-days is a number")?
                }
                "--uid" => a.uid = Some(val("--uid")?.parse().map_err(|_| "--uid is a number")?),
                "--apply" => a.apply = true,
                "--detach" => a.detach = true,
                other => a.rest.push(other.to_string()),
            }
        }
        Ok(a)
    }

    pub fn run(args: Vec<String>) -> i32 {
        let Some(cmd) = args.first().cloned() else {
            eprintln!("usage: cig-vault serve|gc|ask|version (see the source header)");
            return 2;
        };
        let a = match parse(&args[1..]) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("cig-vault: {e}");
                return 2;
            }
        };
        match cmd.as_str() {
            "version" => {
                println!(
                    "cig-vault {} protocol {}",
                    env!("CARGO_PKG_VERSION"),
                    cig_vault::VERSION
                );
                0
            }
            "serve" => serve_cmd(&a),
            "gc" => gc_cmd(&a),
            "ask" => ask_cmd(&a),
            other => {
                eprintln!("cig-vault: unknown command `{other}`");
                2
            }
        }
    }

    fn harden() {
        // SAFETY: plain process settings with no memory involved: no core
        // dump, and (Linux) not dumpable, so nothing attaches to the vault.
        unsafe {
            let z = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &z);
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            libc::umask(0o077);
        }
    }

    fn serve_cmd(a: &Args) -> i32 {
        harden();
        let store = match Store::open(&a.root, a.quota) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!("cig-vault: {e}");
                return 1;
            }
        };
        store.sweep_tmp();
        let listener = match bind(&a.socket) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cig-vault: {e}");
                return 1;
            }
        };
        let opts = Options {
            grace: Duration::from_secs(a.grace_days * 24 * 3600),
            ..Options::default()
        };
        eprintln!(
            "cig-vault: serving {} from {}",
            a.socket.display(),
            a.root.display()
        );
        if a.detach && !detach() {
            return 1;
        }
        serve(listener, store, opts);
        0
    }

    /// Go to the background once the socket is bound: the caller that started
    /// us returns only when the vault is already answering. Before any thread
    /// exists, so the fork is safe.
    fn detach() -> bool {
        // SAFETY: fork in a process that is still single-threaded; the child
        // gets its own session and /dev/null for stdio, the parent leaves.
        unsafe {
            match libc::fork() {
                -1 => {
                    eprintln!("cig-vault: cannot go to the background");
                    false
                }
                0 => {
                    libc::setsid();
                    let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                    if null >= 0 {
                        for fd in 0..3 {
                            libc::dup2(null, fd);
                        }
                    }
                    true
                }
                _ => libc::_exit(0),
            }
        }
    }

    fn gc_cmd(a: &Args) -> i32 {
        let Some(uid) = a.uid else {
            eprintln!("cig-vault gc: name the user's tin with --uid N");
            return 2;
        };
        harden();
        let store = match Store::open(&a.root, a.quota) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cig-vault: {e}");
                return 1;
            }
        };
        let mut live = HashSet::new();
        for l in std::io::stdin().lock().lines().map_while(Result::ok) {
            let mut w = l.split(' ');
            match (w.next().and_then(Kind::parse), w.next()) {
                (Some(k), Some(id)) if k.id_ok(id) => {
                    live.insert((k, id.to_string()));
                }
                _ if l.trim().is_empty() => {}
                _ => {
                    eprintln!("cig-vault gc: a live line is `<kind> <id>`, not `{l}`");
                    return 2;
                }
            }
        }
        // A live set with nothing in it would name everything garbage: a
        // person asking for that says so by removing the tin, not by gc.
        if live.is_empty() {
            eprintln!(
                "cig-vault gc: the live set is empty; refusing to treat everything as garbage"
            );
            return 2;
        }
        let plan = store.gc_plan(uid, &live, Duration::from_secs(a.grace_days * 24 * 3600));
        if a.apply {
            let (n, bytes) = store.gc_apply(uid, &plan);
            println!("removed {n} objects, {bytes} bytes");
        } else {
            for (k, id) in &plan {
                println!("{} {id}", k.letter());
            }
            println!(
                "{} objects would be removed (--apply removes them)",
                plan.len()
            );
        }
        0
    }

    fn ask_cmd(a: &Args) -> i32 {
        let mut c = match client::Client::connect(&a.socket, None) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("cig-vault: {e}");
                return 1;
            }
        };
        let w: Vec<&str> = a.rest.iter().map(String::as_str).collect();
        let kind = w.get(1).and_then(|k| Kind::parse(k));
        let r = match (w.first().copied(), kind, w.get(2)) {
            (Some("list"), Some(k), _) => c.list(k).map(|items| {
                for (id, len) in items {
                    println!("{id} {len}");
                }
            }),
            (Some("has"), Some(k), Some(id)) => c
                .has(k, id)
                .map(|b| println!("{}", if b { "yes" } else { "no" })),
            (Some("verify"), Some(k), Some(id)) => c.verify(k, id).map(|n| println!("ok {n}")),
            _ => {
                eprintln!(
                    "usage: cig-vault ask list <kind> | has <kind> <id> | verify <kind> <id>"
                );
                return 2;
            }
        };
        match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("cig-vault: {e}");
                1
            }
        }
    }
}
