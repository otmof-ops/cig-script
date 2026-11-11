// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The documents keep their promises: every vernacular term resolves to a
//! lexicon entry with a manual name, every error code a document cites
//! exists, and the wall names only tests that exist.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn readme_lexicon_terms_resolve_to_lexicon_entries_with_a_manual_name() {
    let readme = read("README.md");
    let lexicon = read("docs/LEXICON.md");
    let table_start = readme
        .find("| word | means |")
        .expect("README lexicon table");
    let table: Vec<&str> = readme[table_start..]
        .lines()
        .skip(2)
        .take_while(|l| l.starts_with('|'))
        .collect();
    assert!(
        table.len() >= 10,
        "README lexicon table has {} rows",
        table.len()
    );
    for row in table {
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        assert!(
            cells.len() >= 6,
            "lexicon row needs word | means | in the hallway | because: {row}"
        );
        let word: String = cells[1]
            .trim_matches('`')
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect();
        let word = word.as_str();
        assert!(!cells[3].is_empty(), "no hallway column for {word}: {row}");
        assert!(
            lexicon.contains(&format!("**{word}**")) || lexicon.contains(&format!("**{word} ")),
            "README lexicon word `{word}` has no bold entry in docs/LEXICON.md"
        );
    }
    // The lexicon itself keeps the three-column shape everywhere.
    for line in lexicon.lines().filter(|l| l.starts_with("| **")) {
        let cells = line.matches('|').count();
        assert!(cells >= 4, "lexicon row lacks a manual-name column: {line}");
    }
}

#[test]
fn every_error_code_cited_in_the_docs_exists() {
    let mut cited = BTreeSet::new();
    let mut files = vec![root().join("README.md"), root().join("CONTRIBUTING.md")];
    for e in fs::read_dir(root().join("docs")).unwrap().flatten() {
        // LEXICON.md is Jay's document; ERRORS.md is generated from the registry
        // and cites range bounds. Both are outside this check.
        if e.path().extension().is_some_and(|x| x == "md")
            && e.file_name() != "LEXICON.md"
            && e.file_name() != "ERRORS.md"
        {
            files.push(e.path());
        }
    }
    for f in files {
        let text = fs::read_to_string(&f).unwrap();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i + 4 <= bytes.len() {
            if bytes[i] == b'E'
                && bytes[i + 1..i + 4].iter().all(u8::is_ascii_digit)
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
                && (i + 4 == bytes.len() || !bytes[i + 4].is_ascii_alphanumeric())
            {
                cited.insert((
                    text[i..i + 4].to_string(),
                    f.file_name().unwrap().to_string_lossy().to_string(),
                ));
            }
            i += 1;
        }
    }
    let missing: Vec<_> = cited
        .iter()
        .filter(|(c, _)| cigscript::errors::lookup(c).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "documents cite codes the registry does not have: {missing:?}"
    );
}

#[test]
fn the_wall_names_only_tests_that_exist() {
    let readme = read("README.md");
    let start = readme.find("### The wall").expect("the wall");
    let rows: Vec<&str> = readme[start..]
        .lines()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .filter(|l| !l.starts_with("|---") && !l.starts_with("| cheat"))
        .collect();
    assert!(rows.len() >= 10, "the wall has {} rows", rows.len());
    let mut suite = String::new();
    for f in [
        "tests/cli.rs",
        "tests/wall.rs",
        "tests/hops.rs",
        "tests/pack.rs",
        "tests/registry.rs",
        "src/burn/journal.rs",
        "src/burn/tin.rs",
    ] {
        suite.push_str(&read(f));
    }
    for row in rows {
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let test = cells.get(4).copied().unwrap_or("").trim_matches('`');
        let result = cells.get(2).copied().unwrap_or("");
        if result.starts_with("**open") {
            assert!(
                test.is_empty() || test == "-",
                "an open row must not claim a test: {row}"
            );
            continue;
        }
        assert!(!test.is_empty(), "a closed row must name its test: {row}");
        assert!(
            suite.contains(&format!("fn {test}(")),
            "the wall names a test that does not exist: `{test}`"
        );
    }
}
