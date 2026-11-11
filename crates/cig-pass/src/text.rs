// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Caveats as people write them: `within ~/work/site`, `for 2h`, `once`.

use crate::encode::{Caveat, Network};
use crate::fsid;
use std::path::Path;

/// A caveat as text: what `cig pass show` prints and `narrow` reads back.
pub fn caveat_text(c: &Caveat) -> String {
    match c {
        Caveat::Within(p) => format!("within {}", fsid::shown(p)),
        Caveat::Until(t) => format!("until {}", iso(*t)),
        Caveat::Boxed => "boxed".to_string(),
        Caveat::Network(Network::None) => "network none".to_string(),
        Caveat::Network(Network::Hosts(h)) => format!("network {}", h.join(",")),
        Caveat::Args(g) => format!("args {}", g.join(" ")),
        Caveat::CigAtLeast(v) => format!("cig >= {v}"),
        Caveat::Build(b) => format!("build {b}"),
        Caveat::Agent(a) => format!("agent {a}"),
        Caveat::Once => "once".to_string(),
        Caveat::Max(n) => format!("max {n}"),
        Caveat::Needs(k) => format!("needs {k}"),
    }
}

/// Read a caveat. `~` is `home`; `for <span>` becomes `until` (now + span),
/// with `now_ms` the time of narrowing; a relative `within` is resolved
/// against `cwd`.
pub fn parse_caveat(
    text: &str,
    home: Option<&Path>,
    cwd: &Path,
    now_ms: i64,
) -> Result<Caveat, String> {
    let t = text.trim();
    let (word, rest) = match t.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (t, ""),
    };
    let need = |what: &str| -> Result<&str, String> {
        if rest.is_empty() {
            Err(format!("`{word}` takes {what}"))
        } else {
            Ok(rest)
        }
    };
    let none_after = |c: Caveat| -> Result<Caveat, String> {
        if rest.is_empty() {
            Ok(c)
        } else {
            Err(format!("`{word}` takes nothing after it"))
        }
    };
    let c = match word {
        "within" => {
            let raw = need("a folder")?;
            let expanded = if raw == "~" || raw.starts_with("~/") {
                let h = home.ok_or("`~` needs a home folder, and there is none here")?;
                if raw == "~" {
                    h.to_path_buf()
                } else {
                    h.join(&raw[2..])
                }
            } else {
                Path::new(raw).to_path_buf()
            };
            let abs = if expanded.is_absolute() {
                expanded
            } else {
                cwd.join(expanded)
            };
            let b = fsid::path_bytes(&abs).ok_or("that folder's name cannot be recorded")?;
            Caveat::Within(b)
        }
        "until" => Caveat::Until(parse_time(need("a time (2026-11-14T00:00:00Z)")?)?),
        "for" => {
            let span = parse_span(need("a span (30m, 2h, 7d)")?)?;
            Caveat::Until(now_ms.saturating_add(span))
        }
        "boxed" => none_after(Caveat::Boxed)?,
        "network" => {
            let v = need("`none` or hosts")?;
            if v == "none" {
                Caveat::Network(Network::None)
            } else {
                let hosts: Vec<String> = v
                    .split(',')
                    .map(|h| h.trim().to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
                    .collect();
                if hosts.is_empty() {
                    return Err("`network` takes `none` or hosts separated by commas".to_string());
                }
                Caveat::Network(Network::Hosts(hosts))
            }
        }
        "args" => {
            let globs: Vec<String> = need("one pattern per argument")?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            for (n, g) in globs.iter().enumerate() {
                if g == "..." && n + 1 != globs.len() {
                    return Err("`...` stands only at the end of `args`".to_string());
                }
                if g != "..." {
                    globset::Glob::new(g)
                        .map_err(|e| format!("the pattern `{g}` does not read: {e}"))?;
                }
            }
            Caveat::Args(globs)
        }
        "cig" => {
            let v = rest
                .strip_prefix(">=")
                .map(str::trim)
                .ok_or("write `cig >= 1.20.0`")?;
            if crate::verify::version_parts(v).is_none() {
                return Err(format!("`{v}` is not a version"));
            }
            Caveat::CigAtLeast(v.to_string())
        }
        "build" => Caveat::Build(need("a build id")?.to_string()),
        "agent" => Caveat::Agent(need("an agent's name")?.to_string()),
        "once" => none_after(Caveat::Once)?,
        "max" => {
            let n: u32 = need("a count")?
                .parse()
                .map_err(|_| "`max` takes a whole number".to_string())?;
            if n == 0 {
                return Err("`max 0` would never hold".to_string());
            }
            Caveat::Max(n)
        }
        "needs" => {
            let k = need("a kind of pass")?;
            if !crate::encode::is_kind(k) {
                return Err(format!("`{k}` is not a kind of pass"));
            }
            Caveat::Needs(k.to_string())
        }
        "" => return Err("an empty caveat".to_string()),
        other => {
            return Err(format!(
                "`{other}` is not a caveat (within, until, for, boxed, network, args, cig >=, build, agent, once, max, needs)"
            ))
        }
    };
    crate::issue::check_caveat(&c)?;
    Ok(c)
}

/// `30s`, `30m`, `2h`, `7d`, `1w` in milliseconds.
pub fn parse_span(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: i64 = n
        .parse()
        .map_err(|_| format!("`{s}` is not a span (30m, 2h, 7d)"))?;
    let mul = match unit.trim() {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 7 * 86_400_000,
        _ => return Err(format!("`{s}` is not a span (30m, 2h, 7d)")),
    };
    n.checked_mul(mul)
        .ok_or_else(|| format!("`{s}` is too long"))
}

/// `2026-11-14T00:00:00Z` (or with milliseconds, or a date alone) in unix
/// milliseconds. UTC only, so a time means the same everywhere.
pub fn parse_time(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let bad = || format!("`{s}` is not a UTC time (2026-11-14T00:00:00Z)");
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t.strip_suffix('Z').ok_or_else(bad)?)),
        None => (s, None),
    };
    let d: Vec<&str> = date.split('-').collect();
    if d.len() != 3 {
        return Err(bad());
    }
    let (y, mo, da): (i64, i64, i64) = (
        d[0].parse().map_err(|_| bad())?,
        d[1].parse().map_err(|_| bad())?,
        d[2].parse().map_err(|_| bad())?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return Err(bad());
    }
    let (mut h, mut mi, mut se, mut ms) = (0i64, 0i64, 0i64, 0i64);
    if let Some(t) = time {
        let (hms, frac) = match t.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (t, None),
        };
        let p: Vec<&str> = hms.split(':').collect();
        if p.len() != 3 {
            return Err(bad());
        }
        h = p[0].parse().map_err(|_| bad())?;
        mi = p[1].parse().map_err(|_| bad())?;
        se = p[2].parse().map_err(|_| bad())?;
        if let Some(f) = frac {
            let f3: String = f.chars().chain("000".chars()).take(3).collect();
            ms = f3.parse().map_err(|_| bad())?;
        }
        if h > 23 || mi > 59 || se > 60 {
            return Err(bad());
        }
    }
    let days = days_from_civil(y, mo, da);
    Ok(((days * 24 + h) * 60 + mi) * 60_000 + se * 1000 + ms)
}

/// A unix time in ms as `2026-11-14T00:00:00Z`.
pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// Howard Hinnant's civil-date algorithms.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
