// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig explain [code]`: the registry, rendered.
//!
//! No argument lists every code by range. A code prints its page: which
//! kind of no it is (the hallway phrase first, the manual's name in
//! brackets), what it means, what to type next, the ranked causes with the
//! probe doctor would use and the remedy for each, and the related codes.
//! `--schema` prints the JSON Schema for a diagnostic; `--json` prints the
//! registry nodes themselves.

use super::{exit, Ctx};
use crate::errors::{all, diagnostic_schema, lookup, lookup_probe, registry, Code};

pub fn explain(ctx: &Ctx, code: Option<String>, schema: bool) -> i32 {
    if schema {
        outln!(
            "{}",
            serde_json::to_string_pretty(&diagnostic_schema()).unwrap_or_default()
        );
        return exit::OK;
    }
    match code {
        None => list(ctx),
        Some(code) => match lookup(&code) {
            Some(entry) => {
                if ctx.json {
                    outln!("{}", entry.to_json());
                } else {
                    page(ctx, entry);
                }
                exit::OK
            }
            None => {
                eprintln!(
                    "{} no error code `{code}`; codes look like E502. `cig explain` lists them all.",
                    ctx.red("error[E800 usage]:")
                );
                exit::USAGE
            }
        },
    }
}

fn list(ctx: &Ctx) -> i32 {
    if ctx.json {
        let nodes: Vec<serde_json::Value> = all().iter().map(Code::to_json).collect();
        outln!(
            "{}",
            serde_json::to_string_pretty(&nodes).unwrap_or_default()
        );
        return exit::OK;
    }
    outln!("{}", ctx.bold("CigScript error codes"));
    outln!(
        "{}",
        ctx.dim("a code never changes meaning; a retired code is never reused")
    );
    for range in &registry().ranges {
        let codes: Vec<&Code> = all()
            .iter()
            .filter(|c| range.holds(crate::errors::number(&c.code).unwrap_or(0)))
            .collect();
        outln!();
        outln!(
            "{}  {}",
            ctx.bold(&format!("{}-{}", range.from, range.to)),
            range.meaning
        );
        if codes.is_empty() {
            outln!("  {}", ctx.dim("reserved; nothing emitted yet"));
        }
        for c in codes {
            let mark = if c.has_tag("planned") {
                ctx.dim("  (planned)")
            } else if c.has_tag("orchestrator") {
                ctx.dim("  (cigo)")
            } else {
                String::new()
            };
            outln!(
                "  {}  {:<34} {}{mark}",
                ctx.bold(&c.code),
                c.title,
                ctx.dim(c.kind().as_str())
            );
        }
    }
    outln!();
    outln!(
        "{}",
        ctx.dim("cig explain <code> for the page; cig explain --schema for the JSON schema")
    );
    exit::OK
}

fn page(ctx: &Ctx, e: &Code) {
    let no = e.no_entry();
    outln!(
        "{} {}  {}",
        ctx.bold(&e.code),
        ctx.bold(&e.title),
        ctx.dim(&format!(
            "({}, {} family, since {})",
            e.kind().as_str(),
            e.family(),
            e.since
        ))
    );
    if e.retired {
        outln!(
            "  {}",
            ctx.yellow("retired: kept so old run records still decode; never emitted again")
        );
    }
    if e.has_tag("planned") {
        outln!(
            "  {}",
            ctx.dim("planned: reserved for a feature that has not shipped yet")
        );
    }
    if e.has_tag("orchestrator") {
        outln!(
            "  {}",
            ctx.dim("from cig-orchestrator: its guard reports it, cig itself never does")
        );
    }
    outln!();
    if ctx.plain {
        outln!(
            "  {} {}  {}",
            ctx.yellow("kind of no:"),
            no.manual,
            ctx.dim(&format!("({})", no.whose))
        );
    } else {
        outln!(
            "  {} {}  {}",
            ctx.yellow("kind of no:"),
            no.hear,
            ctx.dim(&format!("({}; {})", no.whose, no.manual))
        );
    }
    outln!();
    outln!("  {}", e.meaning);
    outln!();
    outln!("  {} {}", ctx.green("what to type next:"), e.fix);
    if !e.causes.is_empty() {
        outln!();
        outln!(
            "  {} {}",
            ctx.bold("known causes,"),
            ctx.dim("ranked; doctor checks them in this order")
        );
        for (i, c) in e.causes.iter().enumerate() {
            match c.no.as_deref().and_then(crate::errors::lookup_no) {
                Some(n) if ctx.plain => outln!(
                    "  {}. {}  {}",
                    i + 1,
                    c.why,
                    ctx.dim(&format!("({})", n.manual))
                ),
                Some(n) => outln!(
                    "  {}. {}  {}",
                    i + 1,
                    c.why,
                    ctx.dim(&format!("({}; {})", n.hear, n.manual))
                ),
                None => outln!("  {}. {}", i + 1, c.why),
            }
            let probe = lookup_probe(&c.probe);
            match probe {
                Some(p) if p.name != "none" => {
                    outln!(
                        "     {} {} {}",
                        ctx.dim("probe:"),
                        p.name,
                        ctx.dim(&format!("({})", p.reads))
                    )
                }
                _ => outln!(
                    "     {} {}",
                    ctx.dim("probe:"),
                    ctx.dim("none; a suggestion, not a finding")
                ),
            }
            let mut flags = Vec::new();
            if c.fixable {
                flags.push("doctor can apply it with a y");
            }
            if c.needs_burn {
                flags.push("needs a burn");
            }
            if !c.reversible {
                flags.push("not reversible");
            }
            if flags.is_empty() {
                outln!("     {} {}", ctx.green("remedy:"), c.remedy);
            } else {
                outln!(
                    "     {} {}  {}",
                    ctx.green("remedy:"),
                    c.remedy,
                    ctx.dim(&format!("[{}]", flags.join("; ")))
                );
            }
        }
    }
    let mut footer = Vec::new();
    if !e.arises.is_empty() {
        footer.push(format!("arises in: {}", e.arises.join(", ")));
    }
    if !e.related.is_empty() {
        footer.push(format!("related: {}", e.related.join(", ")));
    }
    let tags: Vec<&str> = e
        .tags
        .iter()
        .map(String::as_str)
        .filter(|t| *t != "planned" && *t != "orchestrator")
        .collect();
    if !tags.is_empty() {
        footer.push(format!("tags: {}", tags.join(", ")));
    }
    if !footer.is_empty() {
        outln!();
        outln!("  {}", ctx.dim(&footer.join("  ·  ")));
    }
}
