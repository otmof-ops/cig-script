// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Where the time went, as a small versioned line format (#179 step 4).
//!
//! A product writes what it measured about itself to the descriptor named by
//! `CIG_TIMINGS_FD` (and its counters to the one named by `CIG_STATS_FD`; the
//! two may name the same descriptor). `cig bench --timings` and `--stats` set
//! them for one extra, untimed run of a case and read the lines back. `cig run
//! --timings[=FD]` and `--stats[=FD]` ask for the same by hand; with no `=FD`
//! and no variable the lines go to stderr.
//!
//! The format is one JSON object per line, version 1:
//!
//! ```text
//! {"v":1,"product":"cig","phase":"check","ms":0.412}
//! {"v":1,"product":"cig","counter":"allocs","value":18231}
//! {"v":1,"product":"cig","counter":"allocs","unavailable":"built without the bench-stats feature"}
//! ```
//!
//! Phases are flat, in the order they ended, and add up to at most the
//! process's wall time; what is left over is the reader's `other`. A reader
//! ignores keys it does not know and refuses a line whose `v` it does not
//! know. The full description is in `docs/BENCH.md`.
//!
//! With no variable and no flag nothing here reads a clock or writes a byte:
//! [`Timings::start`] returns `None` and [`Timings::end`] returns at once.

use std::sync::OnceLock;
use std::time::Instant;

static ENTRY: OnceLock<Instant> = OnceLock::new();

/// Called first thing when `cig` starts, so the `start` phase (arguments,
/// configuration, reading the script) has a beginning.
pub fn mark_entry() {
    let _ = ENTRY.set(Instant::now());
}

/// The line format's version.
pub const VERSION: u32 = 1;
/// The environment variable that names the descriptor for phases.
pub const TIMINGS_ENV: &str = "CIG_TIMINGS_FD";
/// The environment variable that names the descriptor for counters.
pub const STATS_ENV: &str = "CIG_STATS_FD";
/// The name written in every line's `product` key.
const PRODUCT: &str = "cig";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sink {
    Fd(i32),
    Stderr,
}

/// A run's timings and counters sinks. Both are off unless asked for.
#[derive(Debug)]
pub struct Timings {
    phases: Option<Sink>,
    stats: Option<Sink>,
    /// `--stats` was asked for by flag, so an unavailable counter is said
    /// aloud (not only written as a line).
    stats_by_flag: bool,
}

impl Timings {
    /// Nothing is recorded.
    pub fn off() -> Timings {
        Timings {
            phases: None,
            stats: None,
            stats_by_flag: false,
        }
    }

    /// From `cig run --timings[=FD]` and `--stats[=FD]` (`Some("")` is the
    /// flag with no value) and the two variables. A variable that names no
    /// open descriptor is ignored: a stale variable never breaks a run. A
    /// flag's bad descriptor is a usage error. The variables are removed from
    /// this process's environment, so a child cig does not write the same
    /// phases a second time inside this one's `run`.
    pub fn from_run(timings: Option<&str>, stats: Option<&str>) -> Result<Timings, String> {
        let phases = resolve(timings, TIMINGS_ENV)?;
        let stats_sink = resolve(stats, STATS_ENV)?;
        for var in [TIMINGS_ENV, STATS_ENV] {
            if std::env::var_os(var).is_some() {
                std::env::remove_var(var);
            }
        }
        let t = Timings {
            phases,
            stats: stats_sink,
            stats_by_flag: stats.is_some(),
        };
        // Everything from the start of `entry` to here.
        t.end("start", ENTRY.get().copied());
        Ok(t)
    }

    pub fn active(&self) -> bool {
        self.phases.is_some() || self.stats.is_some()
    }

    /// Start a phase. `None` (and no clock read) when phases are off.
    #[inline]
    pub fn start(&self) -> Option<Instant> {
        self.phases.map(|_| Instant::now())
    }

    /// End a phase begun with [`Timings::start`] and write its line.
    #[inline]
    pub fn end(&self, phase: &str, started: Option<Instant>) {
        let (Some(sink), Some(t0)) = (self.phases, started) else {
            return;
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        write_line(
            sink,
            &serde_json::json!({
                "v": VERSION, "product": PRODUCT, "phase": phase,
                "ms": (ms * 1000.0).round() / 1000.0,
            }),
        );
    }

    /// Write the counters, once, at the end of the run.
    pub fn finish(&self) {
        let Some(sink) = self.stats else { return };
        match crate::benchstats::snapshot() {
            Some(s) => {
                for (name, value) in [
                    ("allocs", s.allocs),
                    ("alloc_bytes", s.bytes),
                    ("frees", s.frees),
                ] {
                    write_line(
                        sink,
                        &serde_json::json!({
                            "v": VERSION, "product": PRODUCT, "counter": name, "value": value,
                        }),
                    );
                }
            }
            None => {
                write_line(
                    sink,
                    &serde_json::json!({
                        "v": VERSION, "product": PRODUCT, "counter": "allocs",
                        "unavailable": crate::benchstats::UNAVAILABLE,
                    }),
                );
                if self.stats_by_flag && sink != Sink::Stderr {
                    eprintln!("cig: --stats: {}", crate::benchstats::UNAVAILABLE_LONG);
                }
            }
        }
    }
}

fn resolve(flag: Option<&str>, var: &str) -> Result<Option<Sink>, String> {
    match flag {
        Some("") => Ok(Some(from_var(var).unwrap_or(Sink::Stderr))),
        Some(n) => {
            let fd: i32 = n
                .parse()
                .map_err(|_| format!("`{n}` is not a descriptor number"))?;
            if !is_open(fd) {
                return Err(format!(
                    "descriptor {fd} is not open (hand it over as `{fd}>file`)"
                ));
            }
            Ok(Some(Sink::Fd(fd)))
        }
        None => Ok(from_var(var)),
    }
}

fn from_var(var: &str) -> Option<Sink> {
    let fd: i32 = std::env::var(var).ok()?.trim().parse().ok()?;
    is_open(fd).then_some(Sink::Fd(fd))
}

/// A descriptor this process may write phases to: not stdin, stdout or
/// stderr, and open.
#[cfg(unix)]
fn is_open(fd: i32) -> bool {
    // SAFETY: F_GETFD reads the descriptor's flags and nothing else.
    fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1
}

#[cfg(not(unix))]
fn is_open(_fd: i32) -> bool {
    false
}

fn write_line(sink: Sink, v: &serde_json::Value) {
    let mut line = v.to_string();
    line.push('\n');
    match sink {
        Sink::Stderr => eprint!("{line}"),
        #[cfg(unix)]
        Sink::Fd(fd) => {
            // One write per line: a line is far under PIPE_BUF, so lines from
            // processes sharing the descriptor never interleave. The
            // descriptor is borrowed, never closed; a failed write is dropped.
            // SAFETY: the buffer is valid for its length for the whole call.
            let _ = unsafe { libc::write(fd, line.as_ptr().cast(), line.len()) };
        }
        #[cfg(not(unix))]
        Sink::Fd(_) => {}
    }
}

/// One line read back by a harness.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    Phase {
        product: String,
        phase: String,
        ms: f64,
    },
    Counter {
        product: String,
        counter: String,
        value: f64,
    },
    Unavailable {
        product: String,
        counter: String,
        why: String,
    },
}

/// Read a file of lines. Blank lines are skipped; a line that is not version
/// 1 (or not JSON) is an `Err` naming it, so a product on a newer format is
/// said, not guessed at.
pub fn parse(text: &str) -> (Vec<Reading>, Vec<String>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            bad.push(format!("not JSON: {line}"));
            continue;
        };
        if v["v"].as_u64() != Some(u64::from(VERSION)) {
            bad.push(format!("not timings format {VERSION}: {line}"));
            continue;
        }
        let product = v["product"].as_str().unwrap_or("?").to_string();
        if let (Some(phase), Some(ms)) = (v["phase"].as_str(), v["ms"].as_f64()) {
            ok.push(Reading::Phase {
                product,
                phase: phase.to_string(),
                ms,
            });
        } else if let Some(counter) = v["counter"].as_str() {
            if let Some(value) = v["value"].as_f64() {
                ok.push(Reading::Counter {
                    product,
                    counter: counter.to_string(),
                    value,
                });
            } else if let Some(why) = v["unavailable"].as_str() {
                ok.push(Reading::Unavailable {
                    product,
                    counter: counter.to_string(),
                    why: why.to_string(),
                });
            } else {
                bad.push(format!("a counter with no value: {line}"));
            }
        } else {
            bad.push(format!("neither a phase nor a counter: {line}"));
        }
    }
    (ok, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_read_back_and_odd_ones_named() {
        let text = "{\"v\":1,\"product\":\"cig\",\"phase\":\"check\",\"ms\":1.5}\n\
                    \n\
                    {\"v\":1,\"product\":\"cig\",\"counter\":\"allocs\",\"value\":7}\n\
                    {\"v\":1,\"product\":\"cig\",\"counter\":\"allocs\",\"unavailable\":\"no\"}\n\
                    {\"v\":2,\"product\":\"cigo\",\"phase\":\"x\",\"ms\":1}\n\
                    {\"v\":1,\"product\":\"cigo\",\"phase\":\"x\",\"ms\":2,\"future\":true}\n\
                    garbage\n";
        let (ok, bad) = parse(text);
        assert_eq!(ok.len(), 4, "{ok:?}");
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert!(bad[0].contains("not timings format 1"), "{bad:?}");
        assert!(
            matches!(&ok[3], Reading::Phase { product, ms, .. } if product == "cigo" && *ms == 2.0)
        );
    }

    #[test]
    fn off_reads_no_clock() {
        let t = Timings::off();
        assert!(t.start().is_none());
        assert!(!t.active());
        t.end("check", None);
        t.finish();
    }

    #[test]
    fn a_bad_flag_descriptor_is_refused_and_a_stale_variable_is_not() {
        assert!(resolve(Some("x"), "CIG_TEST_NO_SUCH_VAR").is_err());
        assert!(resolve(Some("1"), "CIG_TEST_NO_SUCH_VAR").is_err());
        assert_eq!(resolve(None, "CIG_TEST_NO_SUCH_VAR"), Ok(None));
        assert_eq!(
            resolve(Some(""), "CIG_TEST_NO_SUCH_VAR"),
            Ok(Some(Sink::Stderr))
        );
    }
}
