// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench --family`, `--with` and `--matrix` (#179 step 7): every
//! product's suite in one report, and one product swapped under the others.
//!
//! **Finding the products.** For each of cig, cigo, cigcompile, cigar and
//! ciginstaller, in this order:
//!
//! 1. `--product NAME=PATH` (a checkout, its `bench` folder, or a manifest);
//! 2. `[products]` in `bench-family.toml` under the config folder
//!    (`$XDG_CONFIG_HOME/cigscript`, else `~/.config/cigscript`): the same
//!    paths;
//! 3. the installer's layout: `NAME` on `PATH` as `<prefix>/bin/NAME`, its
//!    suite at `<prefix>/share/NAME/bench/bench.toml`.
//!
//! A product that is not found is named and skipped; no product at all is a
//! usage error. A `--product` name outside the five is a product like any
//! other, so a repository can be measured that the list has not met yet.
//!
//! **One product's binary** is the manifest's `[bench].binary` (here relative
//! to the checkout's root, the folder above `bench/`), else `NAME` on `PATH`
//! (cig: the running cig).
//!
//! **Swapping.** Every product's binary is put, under its own name, in a
//! folder that goes first on the `PATH` of every command a suite runs, so
//! cigo's suite finds the `cig` it is meant to find: the one the family
//! names, or the one `--with cig@...` swapped in. A command given by
//! absolute path is not swapped. `--with cig@1.16.0` fetches that release
//! the way `--against` does (sha256 checked before anything runs; cig only);
//! anything else names a file. `--matrix cig=1.16.0,1.18.0` is every suite
//! once per value and one column each.

use super::{
    ab, cases_json, conditions_json, exit_for, parse_manifest, print_markdown, print_table,
    report_failures, run_case, scratch_root, Conditions, Ctx, Defaults, Manifest, Opts, Outcome,
    Row,
};
use crate::cli::exit;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The products of the family, in the order they are looked for.
pub(super) const KNOWN: &[&str] = &["cig", "cigo", "cigcompile", "cigar", "ciginstaller"];

struct Product {
    name: String,
    manifest: Manifest,
    manifest_path: PathBuf,
    /// The binary the suite measures when nothing is swapped.
    binary: Option<PathBuf>,
}

/// A swapped-in binary and how it was asked for.
#[derive(Clone, Debug)]
struct Swap {
    label: String,
    binary: PathBuf,
}

pub(super) fn family(ctx: &Ctx, o: &Opts) -> i32 {
    let usage = |e: String| {
        eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
        exit::USAGE
    };
    if o.save.is_some() || o.against.is_some() || o.watch {
        return usage("`--family` does not combine with `--save`, `--against` or `--watch`".into());
    }
    let (products, skipped) = match discover(o) {
        Ok(x) => x,
        Err(e) => return usage(e),
    };
    for s in &skipped {
        eprintln!("bench: {s}");
    }
    if products.is_empty() {
        return usage(
            "no product's suite was found: install the products (`<prefix>/bin/NAME` with `<prefix>/share/NAME/bench/bench.toml`), name checkouts in ~/.config/cigscript/bench-family.toml, or give `--product NAME=PATH`"
                .into(),
        );
    }
    let scratch = match scratch_root() {
        Ok(p) => p,
        Err(e) => return usage(format!("cannot make a private scratch folder: {e}")),
    };
    let code = run_family(ctx, o, &products, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    match code {
        Ok(c) => c,
        Err(e) => usage(e),
    }
}

fn run_family(ctx: &Ctx, o: &Opts, products: &[Product], scratch: &Path) -> Result<i32, String> {
    // `--with`: fixed swaps for the whole run.
    let mut fixed: BTreeMap<String, Swap> = BTreeMap::new();
    for w in &o.with {
        let (name, value) = split_spec(w, '@')?;
        fixed.insert(name.clone(), resolve_value(&name, &value, scratch)?);
    }
    // `--matrix`: one product over several values.
    let matrix: Option<(String, Vec<Swap>)> = match &o.matrix {
        None => None,
        Some(m) => {
            let (name, values) = split_spec(m, '=')?;
            let swaps = values
                .split(',')
                .filter(|v| !v.is_empty())
                .map(|v| resolve_value(&name, v, scratch))
                .collect::<Result<Vec<_>, _>>()?;
            if swaps.len() < 2 {
                return Err("`--matrix` needs two or more values, as `cig=1.16.0,1.18.0`".into());
            }
            if fixed.contains_key(&name) {
                return Err(format!("`{name}` is both in `--with` and `--matrix`"));
            }
            Some((name, swaps))
        }
    };
    let conditions = Conditions::here();
    let runs: Vec<(Option<&Swap>, Measured)> = match &matrix {
        None => vec![(None, measure_all(o, products, &fixed, scratch, "0")?)],
        Some((name, values)) => values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut with = fixed.clone();
                with.insert(name.clone(), v.clone());
                Ok((
                    Some(v),
                    measure_all(o, products, &with, scratch, &i.to_string())?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?,
    };
    let mut code = exit::OK;
    for (_, per) in &runs {
        for (_, rows, _, _) in per {
            report_failures(rows);
            if exit_for(rows) != exit::OK {
                code = exit::SCRIPT_ERROR;
            }
        }
    }
    match &matrix {
        None => print_single(ctx, o, &runs[0].1, &fixed, &conditions),
        Some((name, values)) => print_matrix(ctx, products, name, values, &runs, &conditions),
    }
    Ok(code)
}

/// Every product's suite, once, with these swaps in place.
fn measure_all<'a>(
    o: &Opts,
    products: &'a [Product],
    swaps: &BTreeMap<String, Swap>,
    scratch: &Path,
    tag: &str,
) -> Result<Measured<'a>, String> {
    // Every product's binary under its own name, first on the PATH.
    let shims = scratch.join(format!("shims-{tag}"));
    std::fs::create_dir_all(&shims).map_err(|e| format!("cannot make {}: {e}", shims.display()))?;
    let mut binaries: BTreeMap<String, PathBuf> = BTreeMap::new();
    for p in products {
        if let Some(b) = &p.binary {
            binaries.insert(p.name.clone(), b.clone());
        }
    }
    for (name, s) in swaps {
        binaries.insert(name.clone(), s.binary.clone());
    }
    for (name, bin) in &binaries {
        shim(&shims, name, bin)?;
    }
    let mut path_dirs = vec![shims.clone()];
    if let Some(p) = std::env::var_os("PATH") {
        path_dirs.extend(std::env::split_paths(&p));
    }
    let path = std::env::join_paths(path_dirs).map_err(|e| format!("cannot build PATH: {e}"))?;
    let mut out = Vec::new();
    for p in products {
        let m = &p.manifest;
        let mut env = m.defaults.env.clone();
        env.insert("PATH".into(), path.to_string_lossy().into_owned());
        let defaults = Defaults {
            samples: o.samples.or(m.defaults.samples).unwrap_or(9),
            per: o.per.or(m.defaults.per).unwrap_or(5),
            warmup: o.warmup.or(m.defaults.warmup).unwrap_or(1),
            env,
            home: m.bench.home,
            keep: false,
        };
        if defaults.samples == 0 || defaults.per == 0 {
            return Err("`samples` and `per` are at least 1".into());
        }
        let dir = p
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let own = binaries.get(&p.name).cloned();
        let side = super::Side {
            label: p.name.clone(),
            product: p.name.clone(),
            binary: own.clone(),
            dir,
        };
        let rows: Vec<Row> = m
            .cases
            .iter()
            .filter(|c| o.only.as_ref().is_none_or(|s| c.name.contains(s)))
            .filter(|c| {
                o.group
                    .as_ref()
                    .is_none_or(|g| c.group.as_deref() == Some(g.as_str()))
            })
            .map(|c| run_case(&side, scratch, c, &defaults, o_wants(o)))
            .collect();
        let version = product_version(&side, m, &defaults);
        out.push((p, rows, version, own));
    }
    Ok(out)
}

fn o_wants(o: &Opts) -> super::debug::Wants {
    super::debug::Wants {
        timings: o.timings,
        stats: o.stats,
        syscalls: o.syscalls,
        ..Default::default()
    }
}

/// `name` in `dir`, pointing at `bin` (a copy where links are not free).
fn shim(dir: &Path, name: &str, bin: &Path) -> Result<(), String> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let at = dir.join(file);
    let _ = std::fs::remove_file(&at);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(bin, &at);
    #[cfg(not(unix))]
    let made = std::fs::copy(bin, &at).map(|_| ());
    made.map_err(|e| {
        format!(
            "cannot put {} on the path as {}: {e}",
            bin.display(),
            at.display()
        )
    })
}

/// The product's own version line, when its manifest says how to ask.
fn product_version(side: &super::Side, m: &Manifest, defaults: &Defaults) -> Option<String> {
    let argv = side.argv(m.bench.version_cmd.as_ref()?);
    let home = std::env::temp_dir();
    let env = super::full_env(defaults, &BTreeMap::new(), &home);
    let mut c = super::build_command(&argv, &side.dir, &env, &side.dir, None).ok()?;
    let out = c
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

// ----- finding the products --------------------------------------------------

fn split_spec(spec: &str, prefer: char) -> Result<(String, String), String> {
    let at = spec
        .find(prefer)
        .or_else(|| spec.find(['@', '=']))
        .ok_or_else(|| format!("`{spec}` is not PRODUCT{prefer}VALUE"))?;
    let (name, value) = (&spec[..at], &spec[at + 1..]);
    if name.is_empty()
        || value.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("`{spec}` is not PRODUCT{prefer}VALUE"));
    }
    Ok((name.to_string(), value.to_string()))
}

/// What `--with` and `--matrix` name: a file, or (cig only) a release.
fn resolve_value(product: &str, value: &str, scratch: &Path) -> Result<Swap, String> {
    let p = Path::new(value);
    if p.is_file() {
        let abs = std::fs::canonicalize(p).map_err(|e| format!("cannot use {value}: {e}"))?;
        return Ok(Swap {
            label: value.to_string(),
            binary: abs,
        });
    }
    if product == "cig" && ab::looks_like_version(value) {
        let dir = scratch.join(format!("fetch-{}", value.replace(['/', '\\'], "_")));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (bin, _) = ab::fetch_release(value, &dir)?;
        return Ok(Swap {
            label: value.to_string(),
            binary: bin,
        });
    }
    Err(format!(
        "`{value}` for {product} is not a file{}",
        if product == "cig" {
            " or a cig release version"
        } else {
            " (only cig releases can be fetched; give a path)"
        }
    ))
}

fn config_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cigscript").join("bench-family.toml"))
}

/// `[products] NAME = "path"` of the family file; nothing else is allowed.
fn read_config(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(BTreeMap::new());
    };
    let doc: toml::Value = text
        .parse()
        .map_err(|e| format!("{}: not TOML: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for (k, v) in doc.as_table().into_iter().flatten() {
        match (k.as_str(), v) {
            ("format", toml::Value::Integer(1)) => {}
            ("products", toml::Value::Table(t)) => {
                for (name, p) in t {
                    let p = p.as_str().ok_or_else(|| {
                        format!("{}: products.{name} is a path in quotes", path.display())
                    })?;
                    out.insert(name.clone(), p.to_string());
                }
            }
            _ => return Err(format!("{}: unknown key `{k}`", path.display())),
        }
    }
    Ok(out)
}

fn manifest_of(p: &Path) -> Option<PathBuf> {
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    [p.join("bench").join("bench.toml"), p.join("bench.toml")]
        .into_iter()
        .find(|c| c.is_file())
}

fn on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&file))
        .find(|c| c.is_file())
}

fn discover(o: &Opts) -> Result<(Vec<Product>, Vec<String>), String> {
    let mut explicit: BTreeMap<String, String> = BTreeMap::new();
    if let Some(cfg) = config_file() {
        explicit.extend(read_config(&cfg)?);
    }
    for spec in &o.products {
        let (name, path) = split_spec(spec, '=')?;
        explicit.insert(name, path);
    }
    let mut names: Vec<String> = KNOWN.iter().map(|s| s.to_string()).collect();
    for n in explicit.keys() {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    for name in names {
        let (manifest_path, root_bin): (PathBuf, Option<PathBuf>) = match explicit.get(&name) {
            Some(p) => {
                let p = expand_home(p);
                let Some(m) = manifest_of(&p) else {
                    return Err(format!("{name}: no bench/bench.toml under {}", p.display()));
                };
                (m, None)
            }
            None => {
                let exe = if name == "cig" {
                    std::env::current_exe().ok()
                } else {
                    on_path(&name)
                };
                let Some(exe) = exe else {
                    skipped.push(format!("{name}: not installed, skipped"));
                    continue;
                };
                let suite = exe.parent().and_then(Path::parent).map(|prefix| {
                    prefix
                        .join("share")
                        .join(&name)
                        .join("bench")
                        .join("bench.toml")
                });
                match suite {
                    Some(m) if m.is_file() => (m, Some(exe)),
                    other => {
                        skipped.push(format!(
                            "{name}: installed at {} but ships no suite at {}, skipped",
                            exe.display(),
                            other.map(|m| m.display().to_string()).unwrap_or_default()
                        ));
                        continue;
                    }
                }
            }
        };
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
        let manifest =
            parse_manifest(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        if manifest.bench.product != name {
            return Err(format!(
                "{}: this is the suite of `{}`, not `{name}`",
                manifest_path.display(),
                manifest.bench.product
            ));
        }
        // The binary: the manifest's own (from the checkout's root), the
        // installed one, or the one on the PATH.
        let root = manifest_path
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let binary = match (&manifest.bench.binary, root_bin) {
            (Some(b), _) => Some(root.join(b)),
            (None, Some(exe)) => Some(exe),
            (None, None) if name == "cig" => std::env::current_exe().ok(),
            (None, None) => on_path(&name),
        };
        let binary = binary.and_then(|b| std::fs::canonicalize(&b).ok().or(Some(b)));
        out.push(Product {
            name,
            manifest,
            manifest_path,
            binary,
        });
    }
    Ok((out, skipped))
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => PathBuf::from(h).join(rest),
        _ => PathBuf::from(p),
    }
}

// ----- output ----------------------------------------------------------------

type Measured<'a> = Vec<(&'a Product, Vec<Row>, Option<String>, Option<PathBuf>)>;

fn swaps_line(swaps: &BTreeMap<String, Swap>) -> String {
    if swaps.is_empty() {
        return String::new();
    }
    let v: Vec<String> = swaps
        .iter()
        .map(|(n, s)| format!("{n} = {}", s.label))
        .collect();
    format!(" (swapped: {})", v.join(", "))
}

fn print_single(
    ctx: &Ctx,
    o: &Opts,
    per: &Measured,
    swaps: &BTreeMap<String, Swap>,
    c: &Conditions,
) {
    if ctx.json {
        let products: Vec<serde_json::Value> = per
            .iter()
            .map(|(p, rows, version, bin)| {
                serde_json::json!({
                    "product": p.name,
                    "version": version,
                    "binary": bin.as_ref().map(|b| b.display().to_string()),
                    "manifest": p.manifest_path.display().to_string(),
                    "cases": cases_json(rows),
                })
            })
            .collect();
        let swapped: BTreeMap<&String, serde_json::Value> = swaps
            .iter()
            .map(|(n, s)| (n, serde_json::json!({ "value": s.label, "binary": s.binary.display().to_string() })))
            .collect();
        let doc = serde_json::json!({
            "format": super::FORMAT,
            "family": true,
            "cig_version": env!("CARGO_PKG_VERSION"),
            "conditions": conditions_json(c),
            "swapped": swapped,
            "products": products,
        });
        outln!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return;
    }
    outln!(
        "cig bench --family: {} products{}",
        per.len(),
        swaps_line(swaps)
    );
    for (p, rows, version, bin) in per {
        outln!("");
        outln!(
            "== {}{}{}",
            p.name,
            version
                .as_ref()
                .map(|v| format!(", {v}"))
                .unwrap_or_default(),
            bin.as_ref()
                .map(|b| format!(", binary {}", b.display()))
                .unwrap_or_default()
        );
        if o.markdown {
            print_markdown(&p.manifest, rows, c);
        } else {
            print_table(&p.manifest, rows, c);
        }
    }
}

fn median_cell(o: &Outcome) -> String {
    match o {
        Outcome::Timed(s) => format!("{:.1}", s.median_ms),
        Outcome::Wrong(_) => "WRONG".into(),
        Outcome::Absent(_) => "-".into(),
    }
}

fn print_matrix(
    ctx: &Ctx,
    products: &[Product],
    name: &str,
    values: &[Swap],
    runs: &[(Option<&Swap>, Measured)],
    c: &Conditions,
) {
    if ctx.json {
        let out: Vec<serde_json::Value> = products
            .iter()
            .enumerate()
            .map(|(pi, p)| {
                let cases: Vec<serde_json::Value> = runs[0].1[pi]
                    .1
                    .iter()
                    .enumerate()
                    .map(|(ci, r)| {
                        let cols: Vec<serde_json::Value> = runs
                            .iter()
                            .zip(values)
                            .map(|((_, per), v)| {
                                let row = &per[pi].1[ci];
                                let one = cases_json(std::slice::from_ref(row));
                                serde_json::json!({ "value": v.label, "outcome": one[0]["outcome"] })
                            })
                            .collect();
                        serde_json::json!({ "name": r.name, "group": r.group, "values": cols })
                    })
                    .collect();
                serde_json::json!({ "product": p.name, "cases": cases })
            })
            .collect();
        let doc = serde_json::json!({
            "format": super::FORMAT,
            "family": true,
            "matrix": { "product": name, "values": values.iter().map(|v| v.label.clone()).collect::<Vec<_>>() },
            "cig_version": env!("CARGO_PKG_VERSION"),
            "conditions": conditions_json(c),
            "products": out,
        });
        outln!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return;
    }
    outln!("cig bench --family --matrix {name}: {}", c.line());
    for (pi, p) in products.iter().enumerate() {
        outln!("");
        outln!("== {} (median ms, {name} = ...)", p.name);
        let mut head = format!("{:<30}", "case");
        for v in values {
            head.push_str(&format!(" {:>14}", clip_label(&v.label)));
        }
        head.push_str(&format!(" {:>9}", "last/first"));
        outln!("{head}");
        for (ci, r) in runs[0].1[pi].1.iter().enumerate() {
            let mut line = format!("{:<30}", r.name);
            let mut meds: Vec<Option<f64>> = Vec::new();
            for (_, per) in runs {
                let o = &per[pi].1[ci].outcome;
                line.push_str(&format!(" {:>14}", median_cell(o)));
                meds.push(match o {
                    Outcome::Timed(s) => Some(s.median_ms),
                    _ => None,
                });
            }
            let change = match (
                meds.first().copied().flatten(),
                meds.last().copied().flatten(),
            ) {
                (Some(a), Some(b)) if a > 0.0 => format!("x{:.2}", b / a),
                _ => "-".into(),
            };
            line.push_str(&format!(" {change:>9}"));
            outln!("{line}");
        }
    }
    outln!("");
    outln!("Timings are reported, never gated.");
}

fn clip_label(s: &str) -> String {
    let base = Path::new(s)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .filter(|_| s.contains('/') || s.contains('\\'))
        .unwrap_or_else(|| s.to_string());
    let tail: String = base
        .chars()
        .rev()
        .take(14)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_split_at_the_preferred_mark_or_the_other() {
        assert_eq!(
            split_spec("cigo@./old/cigo", '@').unwrap(),
            ("cigo".into(), "./old/cigo".into())
        );
        assert_eq!(
            split_spec("cig=1.16.0", '@').unwrap(),
            ("cig".into(), "1.16.0".into())
        );
        assert!(split_spec("cig", '@').is_err());
        assert!(split_spec("@x", '@').is_err());
        assert!(split_spec("a b=x", '=').is_err());
    }
}
