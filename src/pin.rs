// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The pin: a script says which cig it was written for, and cig honours it.
//!
//! A pin is a comment, `# cig 1.1`, in the lines before the first
//! statement (a shebang may come first), so every cig ever shipped reads
//! past it. (`# cig: ...` with a colon is a directive other tools use; it
//! counts as a pin only when what follows the colon is a constraint.) A project pins every script at once with a `.cig-version` file
//! beside them or above them; a header in the script wins over the file.
//! An unpinned script runs on whatever cig is installed, as it always did.

use crate::update::Version;
use std::fmt;
use std::path::{Path, PathBuf};

/// What a pin asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Constraint {
    /// `1.1.2`: exactly that one.
    Exact(Version),
    /// `1.1`: any 1.1.x, the newest available.
    Minor(u64, u64),
    /// `1`: any 1.x.
    Major(u64),
    /// `>=1.1.2`: that or newer, within the same major version.
    AtLeast(Version),
}

impl Constraint {
    /// Read a constraint as a person writes it: `1.1`, `1.1.2`, `>=1.1.2`,
    /// with or without a leading `v`.
    pub fn parse(text: &str) -> Result<Constraint, String> {
        let t = text.trim();
        if let Some(rest) = t.strip_prefix(">=") {
            let rest = rest.trim();
            return Version::parse(rest)
                .map(Constraint::AtLeast)
                .ok_or_else(|| format!("`{t}` is not a version constraint"));
        }
        let core = t.trim_start_matches('v');
        if core.is_empty()
            || !core.chars().all(|c| c.is_ascii_digit() || c == '.')
            || core.starts_with('.')
            || core.ends_with('.')
        {
            return Err(format!(
                "`{t}` is not a version constraint; write 1.1 (any 1.1.x), 1.1.2 (exactly) or >=1.1.2"
            ));
        }
        let parts: Vec<&str> = core.split('.').collect();
        let num = |s: &str| {
            s.parse::<u64>()
                .map_err(|_| format!("`{t}` is not a version constraint"))
        };
        match parts.len() {
            1 => Ok(Constraint::Major(num(parts[0])?)),
            2 => Ok(Constraint::Minor(num(parts[0])?, num(parts[1])?)),
            3 => Ok(Constraint::Exact(Version(
                num(parts[0])?,
                num(parts[1])?,
                num(parts[2])?,
            ))),
            _ => Err(format!("`{t}` is not a version constraint")),
        }
    }

    /// Does this cig satisfy the pin? A major version may change the
    /// contract, so no range crosses one.
    pub fn satisfied_by(&self, v: &Version) -> bool {
        match self {
            Constraint::Exact(e) => v == e,
            Constraint::Minor(a, b) => v.0 == *a && v.1 == *b,
            Constraint::Major(a) => v.0 == *a,
            Constraint::AtLeast(m) => v.0 == m.0 && v >= m,
        }
    }

    pub fn is_exact(&self) -> bool {
        matches!(self, Constraint::Exact(_))
    }

    /// The version a release would have to be, at least, for this pin.
    pub fn floor(&self) -> Version {
        match self {
            Constraint::Exact(e) | Constraint::AtLeast(e) => e.clone(),
            Constraint::Minor(a, b) => Version(*a, *b, 0),
            Constraint::Major(a) => Version(*a, 0, 0),
        }
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constraint::Exact(v) => write!(f, "{v}"),
            Constraint::Minor(a, b) => write!(f, "{a}.{b}"),
            Constraint::Major(a) => write!(f, "{a}"),
            Constraint::AtLeast(v) => write!(f, ">={v}"),
        }
    }
}

/// Where a pin came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `# cig: ...` in the script, at this 1-based line.
    Header { line: usize },
    /// A `.cig-version` file at this path.
    VersionFile(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub constraint: Constraint,
    pub source: Source,
}

impl Pin {
    /// For the report: `pinned in s.cig` or `pinned by ../.cig-version`.
    pub fn describe(&self, script: &Path) -> String {
        match &self.source {
            Source::Header { line } => format!(
                "pinned in {} (line {line})",
                script
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            Source::VersionFile(p) => format!("pinned by {}", p.display()),
        }
    }
}

/// The pin line a header carries, if any: the first lines of the script up
/// to the first statement, a shebang allowed. A malformed pin is an error,
/// never ignored: a script that asked for a version it cannot name must not
/// run on whatever is installed.
pub fn header(source: &str) -> Result<Option<(Constraint, usize)>, String> {
    for (i, raw) in source.lines().enumerate() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        if i == 0 && line.starts_with("#!") {
            continue;
        }
        let Some(comment) = line.strip_prefix('#') else {
            break;
        };
        let comment = comment.trim();
        let rest = comment
            .strip_prefix("cig:")
            .or_else(|| comment.strip_prefix("cig "))
            .map(str::trim);
        let Some(rest) = rest else {
            continue;
        };
        // `# cig run s.cig -- x` is a usage line and `# cig: run --dry-run`
        // a harness directive: a pin is what starts like a version.
        if !looks_like_constraint(rest) {
            continue;
        }
        return Constraint::parse(rest)
            .map(|c| Some((c, i + 1)))
            .map_err(|e| format!("line {}: {e}", i + 1));
    }
    Ok(None)
}

fn looks_like_constraint(s: &str) -> bool {
    let s = s.trim_start_matches(">=").trim_start();
    let s = s.strip_prefix('v').unwrap_or(s);
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// The nearest `.cig-version` beside the script or in a directory above it.
pub fn version_file(script: &Path) -> Result<Option<(Constraint, PathBuf)>, String> {
    let abs = if script.is_absolute() {
        script.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(script)
    };
    let mut dir = abs.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        let candidate = d.join(".cig-version");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            let line = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .unwrap_or("");
            return Constraint::parse(line)
                .map(|c| Some((c, candidate.clone())))
                .map_err(|e| format!("{}: {e}", candidate.display()));
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    Ok(None)
}

/// The pin that governs a script: its header, else the project's file.
pub fn pin_for(script: &Path, source: &str) -> Result<Option<Pin>, String> {
    if let Some((constraint, line)) = header(source)? {
        return Ok(Some(Pin {
            constraint,
            source: Source::Header { line },
        }));
    }
    if let Some((constraint, path)) = version_file(script)? {
        return Ok(Some(Pin {
            constraint,
            source: Source::VersionFile(path),
        }));
    }
    Ok(None)
}

/// Write a pin into a script's source: replacing an existing header pin,
/// else under the shebang, else as the first line.
pub fn write(source: &str, constraint: &str) -> String {
    let pin_line = format!("# cig {constraint}");
    let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
    if let Ok(Some((_, line))) = header(source) {
        lines[line - 1] = pin_line;
    } else if lines.first().is_some_and(|l| l.starts_with("#!")) {
        lines.insert(1, pin_line);
    } else {
        lines.insert(0, pin_line);
    }
    let mut out = lines.join("\n");
    if source.ends_with('\n') || source.is_empty() {
        out.push('\n');
    }
    out
}

/// `CIG_DISPATCHED=1` is set on a cig that was handed a run by another: it
/// never dispatches again, so a pin can never loop.
pub fn dispatched() -> bool {
    std::env::var_os("CIG_DISPATCHED").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraints_parse_and_match() {
        let c = Constraint::parse("1.1").unwrap();
        assert!(c.satisfied_by(&Version(1, 1, 0)));
        assert!(c.satisfied_by(&Version(1, 1, 9)));
        assert!(!c.satisfied_by(&Version(1, 2, 0)));
        let c = Constraint::parse("v1.1.2").unwrap();
        assert!(c.satisfied_by(&Version(1, 1, 2)));
        assert!(!c.satisfied_by(&Version(1, 1, 3)));
        let c = Constraint::parse(">= 1.1.2").unwrap();
        assert!(c.satisfied_by(&Version(1, 9, 0)));
        assert!(!c.satisfied_by(&Version(1, 1, 1)));
        assert!(
            !c.satisfied_by(&Version(2, 0, 0)),
            "a pin never crosses a major version"
        );
        assert!(Constraint::parse("1")
            .unwrap()
            .satisfied_by(&Version(1, 5, 0)));
        for bad in ["", "x", "1.", ".1", "1.1.1.1", "~1.1", "1.a"] {
            assert!(Constraint::parse(bad).is_err(), "{bad:?} parsed");
        }
        assert_eq!(Constraint::parse("1.1").unwrap().to_string(), "1.1");
        assert_eq!(Constraint::parse(">=1.1.2").unwrap().to_string(), ">=1.1.2");
    }

    #[test]
    fn a_header_pin_is_read_past_the_shebang_and_stops_at_the_first_statement() {
        let src = "#!/usr/bin/env cig\n# the script\n# cig 1.1\nroll x = 1\n# cig 9.9\n";
        let (c, line) = header(src).unwrap().unwrap();
        assert_eq!(c, Constraint::Minor(1, 1));
        assert_eq!(line, 3);
        assert_eq!(header("roll x = 1\n# cig 1.1\n").unwrap(), None);
        assert!(header("# cig 1.x\nroll x = 1\n").is_err());
        assert!(header("# cig v1.1.1.1\nroll x = 1\n").is_err());
        assert_eq!(header("").unwrap(), None);
        // Usage lines and harness directives are comments, not pins.
        assert_eq!(header("# cig run s.cig -- x\nroll x = 1\n").unwrap(), None);
        assert_eq!(header("# cig: run --dry-run\nroll x = 1\n").unwrap(), None);
        assert_eq!(
            header("# cig: 1.1.2\nroll x = 1\n").unwrap().unwrap().0,
            Constraint::Exact(Version(1, 1, 2))
        );
    }

    #[test]
    fn a_pin_is_written_under_the_shebang() {
        assert_eq!(
            write("#!/usr/bin/env cig\nroll x = 1\n", "1.2"),
            "#!/usr/bin/env cig\n# cig 1.2\nroll x = 1\n"
        );
        assert_eq!(write("roll x = 1\n", "1.2"), "# cig 1.2\nroll x = 1\n");
        assert_eq!(
            write("# cig 1.1\nroll x = 1\n", "1.2"),
            "# cig 1.2\nroll x = 1\n"
        );
    }
}
