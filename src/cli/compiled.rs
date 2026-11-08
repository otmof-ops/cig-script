// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! A compiled program's command line: the flags a `cig run` script takes,
//! then the program's own arguments. See docs/PROGRAMS.md.

use super::{exit, run, Ctx, RunArgs};
use crate::program::{Program, Unrunnable};
use std::path::PathBuf;

/// What a program's command line asked for.
#[derive(Debug, Default, PartialEq)]
struct Opts {
    dry_run: bool,
    no_rollback: bool,
    max_steps: Option<u64>,
    json: bool,
    plain: bool,
    no_color: bool,
    no_doctor: bool,
    unburn: Option<String>,
    explain: Option<String>,
    light: Option<String>,
    seal_key_fd: Option<i32>,
    unburn_last: Option<usize>,
    unburn_since: Option<String>,
    unburn_to: Option<String>,
    chains: bool,
    info: bool,
    help: bool,
    args: Vec<String>,
}

/// Leading flags are the run's; the first word that is not one, or
/// everything after `--`, is the program's own. An unknown flag before
/// that is refused rather than passed on, so a mistyped `--dry-run` never
/// becomes a real run.
fn parse(argv: &[String]) -> Result<Opts, (String, String)> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--" => {
                i += 1;
                break;
            }
            "--dry-run" => o.dry_run = true,
            "--no-rollback" => o.no_rollback = true,
            "--json" => o.json = true,
            "--plain" => o.plain = true,
            "--no-color" => o.no_color = true,
            "--no-doctor" => o.no_doctor = true,
            "--cig-program" => o.info = true,
            "--chains" => o.chains = true,
            "--help" | "-h" => o.help = true,
            "--max-steps" | "--unburn" | "--explain" | "--light" | "--seal-key-fd"
            | "--unburn-last" | "--unburn-since" | "--unburn-to" => {
                let Some(v) = argv.get(i + 1) else {
                    return Err((
                        format!("`{a}` needs a value"),
                        format!("give it one: {a} <value>"),
                    ));
                };
                take(&mut o, a, v)?;
                i += 1;
            }
            _ if a.starts_with("--") && a.contains('=') => {
                let (flag, v) = a.split_once('=').expect("has =");
                if !matches!(
                    flag,
                    "--max-steps"
                        | "--unburn"
                        | "--explain"
                        | "--light"
                        | "--seal-key-fd"
                        | "--unburn-last"
                        | "--unburn-since"
                        | "--unburn-to"
                ) {
                    return Err((
                        format!("`{flag}` is not one of this program's run flags"),
                        format!("the program's own arguments go after `--`: -- {a}"),
                    ));
                }
                take(&mut o, flag, v)?;
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                return Err((
                    format!("`{a}` is not one of this program's run flags"),
                    format!("the program's own arguments go after `--`: -- {a}"),
                ));
            }
            _ => break,
        }
        i += 1;
    }
    o.args = argv[i.min(argv.len())..].to_vec();
    Ok(o)
}

fn take(o: &mut Opts, flag: &str, v: &str) -> Result<(), (String, String)> {
    match flag {
        "--max-steps" => o.max_steps = Some(steps(v)?),
        "--unburn" => o.unburn = Some(v.to_string()),
        "--explain" => o.explain = Some(v.to_string()),
        "--unburn-last" => {
            o.unburn_last = Some(v.parse::<usize>().map_err(|_| {
                (
                    format!("`--unburn-last {v}` is not a number of runs"),
                    "give how many of this program's newest runs to put back: --unburn-last 3"
                        .to_string(),
                )
            })?)
        }
        "--unburn-since" => o.unburn_since = Some(v.to_string()),
        "--unburn-to" => o.unburn_to = Some(v.to_string()),
        "--seal-key-fd" => {
            o.seal_key_fd = Some(v.parse::<i32>().map_err(|_| {
                (
                    format!("`--seal-key-fd {v}` is not a descriptor number"),
                    "give the number the key is open on: --seal-key-fd 3, with 3<key.hex"
                        .to_string(),
                )
            })?)
        }
        _ => o.light = Some(v.to_string()),
    }
    Ok(())
}

fn steps(v: &str) -> Result<u64, (String, String)> {
    v.parse::<u64>().map_err(|_| {
        (
            format!("`--max-steps {v}` is not a step count"),
            "give a whole number of steps, or 0 to disable the budget".to_string(),
        )
    })
}

fn usage_text(p: &Program) -> String {
    format!(
        "{name}: a CigScript program ({file}, compiled by {compiler} for cig {cig})

usage: {name} [run flags] [--] [arguments...]

run flags (before the program's own arguments):
  --dry-run          plan every burn instead of doing it, and print the plan
  --no-rollback      leave the burns in place if the program fails
  --max-steps N      the step budget (0 turns it off; also CIG_MAX_STEPS)
  --json             machine-readable output where the run gives it
  --plain            the same information without the catchphrases
  --no-color         never colour (also NO_COLOR)
  --no-doctor        no automatic diagnosis on errors (also CIG_DOCTOR=0)
  --light <chain>    run the program, then light the chain it declares (cig light)
  --chains           list the chains the program declares (cig chains)
  --unburn <run>     put a past run of this program back (cig unburn)
  --unburn-last N    put back this program's newest N runs, newest first
  --unburn-since AGE put back its runs started within AGE (90s, 30m, 2h, 3d, 1w)
  --unburn-to <run>  put back its runs after that one, newest first
  --seal-key-fd N    read the tin's key from open descriptor N (Unix; CI)
  --explain <code>   what an error code means and what to do (cig explain)
  --cig-program      what this program is, and what it was compiled from
  --                 everything after this is the program's own",
        name = p.name,
        file = p.file,
        compiler = p.compiler,
        cig = p.cig,
    )
}

fn context(o: &Opts) -> Ctx {
    Ctx {
        json: o.json,
        color: !o.no_color && std::env::var_os("NO_COLOR").is_none() && super::stderr_is_tty(),
        doctor: !o.no_doctor && crate::doctor::enabled_by_env(),
        plain: o.plain || super::plain_by_env(),
    }
}

/// Run the program appended to this binary.
pub fn main(p: Program) -> i32 {
    crate::program::set_running();
    let mut argv = Vec::new();
    for (n, a) in std::env::args_os().skip(1).enumerate() {
        match a.into_string() {
            Ok(s) => argv.push(s),
            Err(_) => {
                let d = crate::diagnostics::usage(format!("argument {} is not valid UTF-8", n + 1))
                    .code("E1052")
                    .with_hint("pass it as text, or through a file the program reads");
                run::report(&context(&Opts::default()), &d, None, None);
                return exit::USAGE;
            }
        }
    }
    let o = match parse(&argv) {
        Ok(o) => o,
        Err((message, hint)) => {
            let d = crate::diagnostics::usage(message)
                .code("E1052")
                .with_hint(format!("{hint}; {} --help lists the run flags", p.name));
            run::report(&context(&Opts::default()), &d, None, None);
            return exit::USAGE;
        }
    };
    let ctx = context(&o);
    if o.help {
        outln!("{}", usage_text(&p));
        return exit::OK;
    }
    if let Some(code) = o.explain {
        return super::explain::explain(&ctx, Some(code), false);
    }
    if o.chains {
        return super::chains::chains_source(&ctx, &p.file, &p.source);
    }
    if o.info {
        let mut v = p.summary();
        v["runtime"] = serde_json::json!(crate::VERSION);
        if ctx.json {
            outln!("{}", serde_json::to_string(&v).unwrap_or_default());
        } else {
            outln!("{}  a CigScript program", p.name);
            outln!("  compiled from  {}  (sha256 {})", p.file, p.sha256);
            outln!("  compiled by    {}", p.compiler);
            outln!("  runs on        cig {} (this binary)", crate::VERSION);
        }
        return exit::OK;
    }
    if let Some(fd) = o.seal_key_fd {
        if let Some(code) = super::take_key_fd(&ctx, fd) {
            return code;
        }
    }
    // A stretch of the program's own runs (#149), as cig unburn --last,
    // --since and --to do for any.
    let stretch = match (o.unburn_last, &o.unburn_since, &o.unburn_to) {
        (Some(n), _, _) => Some(super::runs::Stretch::Last(n)),
        (None, Some(age), _) => match super::runs::parse_age(age) {
            Some(d) => Some(super::runs::Stretch::Since(d)),
            None => {
                let d = crate::diagnostics::usage(format!("`--unburn-since {age}` is not an age"))
                    .code("E1052")
                    .with_hint("a number and a unit: 90s, 30m, 2h, 3d or 1w");
                run::report(&ctx, &d, None, None);
                return exit::USAGE;
            }
        },
        (None, None, Some(id)) => Some(super::runs::Stretch::To(id.clone())),
        _ => None,
    };
    if let Some(how) = stretch {
        return super::runs::unburn_stretch(
            &ctx,
            how,
            None,
            super::runs::StretchOpts {
                dry_run: o.dry_run,
                force: false,
                accept_unsigned: false,
                yes: false,
                only_program: Some(p.sha256.clone()),
            },
        );
    }
    if let Some(id) = &o.unburn {
        // A program puts back its own runs only; `cig unburn` does any.
        if let Ok(Some(rec)) = crate::burn::runs::find(id) {
            let ours = rec
                .program
                .as_ref()
                .and_then(|v| v["sha256"].as_str())
                .is_some_and(|sha| sha == p.sha256);
            if !ours {
                let d = crate::diagnostics::usage(format!(
                    "run {} was not made by this program ({})",
                    rec.id, p.name
                ))
                .code("E1053")
                .with_hint(format!("cig unburn {} puts back any run", rec.id));
                run::report(&ctx, &d, None, None);
                return exit::USAGE;
            }
        }
        return super::runs::unburn(&ctx, id, o.dry_run, false, false);
    }
    let args = RunArgs {
        file: PathBuf::from(&p.file),
        dry_run: o.dry_run,
        no_rollback: o.no_rollback,
        // Checked when it was compiled; a program does not lint itself on
        // every start.
        no_check: true,
        max_steps: o.max_steps,
        trial: false,
        no_trial: true,
        args: o.args,
    };
    let code = run::run_program(&ctx, args, &p, o.light);
    crate::ashtray::spawn_flush(&crate::config::Config::load());
    code
}

/// A binary that carries a program it will not run.
pub fn refuse(why: &Unrunnable) -> i32 {
    let ctx = context(&Opts::default());
    let d = match why {
        Unrunnable::Damaged(e) => crate::diagnostics::usage(format!(
            "this program is damaged: {e}"
        ))
        .code("E1050")
        .with_hint(
            "compile it again (cigcompile build), or fetch it again from where it came from",
        ),
        Unrunnable::OtherCig { name, cig } => crate::diagnostics::usage(format!(
            "{name} was compiled for cig {cig}, and its runtime is cig {}",
            crate::VERSION
        ))
        .code("E1051")
        .with_hint(
            "compile it again with the cigcompile for this cig, or keep the binary cigcompile made",
        ),
        Unrunnable::Raised { name } => crate::diagnostics::usage(format!(
            "{name} is running with more rights than whoever started it (setuid, setgid or file capabilities), and a program does not run like that"
        ))
        .code("E1054")
        .with_hint("run it with sudo instead, and take the bits off its file: chmod u-s,g-s, setcap -r"),
        Unrunnable::Unreadable { name } => crate::diagnostics::usage(format!(
            "{name} cannot read its own file, so it cannot find its script (it was installed without read permission)"
        ))
        .code("E1055")
        .with_hint("make it readable by whoever runs it (chmod 755), as cigcompile writes it"),
    };
    run::report(&ctx, &d, None, None);
    exit::USAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn leading_flags_are_the_runs_and_the_rest_is_the_programs() {
        let o = parse(&words(&["--dry-run", "--max-steps", "9", "a", "--json"])).unwrap();
        assert!(o.dry_run);
        assert_eq!(o.max_steps, Some(9));
        assert!(!o.json, "a flag after the first argument is the program's");
        assert_eq!(o.args, words(&["a", "--json"]));
    }

    #[test]
    fn a_double_dash_hands_over_everything_after_it() {
        let o = parse(&words(&["--plain", "--", "--dry-run", "x"])).unwrap();
        assert!(o.plain && !o.dry_run);
        assert_eq!(o.args, words(&["--dry-run", "x"]));
    }

    #[test]
    fn a_mistyped_flag_is_refused_not_passed_on() {
        let (m, hint) = parse(&words(&["--dry-rn"])).unwrap_err();
        assert!(m.contains("--dry-rn"), "{m}");
        assert!(hint.contains("-- --dry-rn"), "{hint}");
    }

    #[test]
    fn values_are_checked() {
        assert!(parse(&words(&["--max-steps", "x"])).is_err());
        assert!(parse(&words(&["--max-steps"])).is_err());
        assert_eq!(
            parse(&words(&["--max-steps=0"])).unwrap().max_steps,
            Some(0)
        );
        assert_eq!(
            parse(&words(&["--unburn=20261107T010203-abcdef"]))
                .unwrap()
                .unburn
                .as_deref(),
            Some("20261107T010203-abcdef")
        );
        assert_eq!(
            parse(&words(&["--seal-key-fd", "3", "a"]))
                .unwrap()
                .seal_key_fd,
            Some(3)
        );
        assert_eq!(
            parse(&words(&["--seal-key-fd=0"])).unwrap().seal_key_fd,
            Some(0)
        );
        assert!(parse(&words(&["--seal-key-fd", "three"])).is_err());
        assert!(parse(&words(&["--seal-key-fd"])).is_err());
    }

    #[test]
    fn the_cig_words_a_program_keeps() {
        let o = parse(&words(&["--explain", "E1050"])).unwrap();
        assert_eq!(o.explain.as_deref(), Some("E1050"));
        let o = parse(&words(&["--light=build", "--chains"])).unwrap();
        assert_eq!(o.light.as_deref(), Some("build"));
        assert!(o.chains);
        assert!(
            parse(&words(&["--color=always"])).is_err(),
            "only the run's own flags take ="
        );
    }

    #[test]
    fn a_lone_dash_is_an_argument() {
        assert_eq!(parse(&words(&["-"])).unwrap().args, words(&["-"]));
        assert_eq!(parse(&words(&[])).unwrap(), Opts::default());
    }
}
