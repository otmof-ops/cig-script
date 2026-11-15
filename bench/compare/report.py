#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
"""report.py: the numbers of a compare.py run, and the documents that quote them.

    report.py --pass DIR [--pass DIR ...] --numbers OUT.json
              [--render TEMPLATE OUT ...]

Every number a document quotes comes from here. A template is Markdown with
two kinds of placeholder:

    {{KEY}}          the text of one number, as numbers.json holds it
    {{table:NAME}}   one of the tables, as Markdown

and nothing else in it is a measurement. check.py, written apart from this
file, computes every key again from the same samples and renders every
template again; the documents are right when its output and this file's are
the same, byte for byte. SPEC.md, beside this file, is what both are written to.
"""
import argparse
import json
import math
import os
import re
import statistics
import sys

START = ["hello", "startup-0", "startup-1", "startup-10"]
CIG_SIDES = ["cig", "stage-a", "stage-b", "cig-dry", "stage-a-dry", "stage-b-dry"]
LANGUAGES = ["lua", "luajit-off", "luajit", "python", "node", "rust"]
IDIOMS = {"python-idiom": "python", "node-idiom": "node", "rust-idiom": "rust"}
MAIN = ["cig", "stage-b"] + LANGUAGES
LABEL = {
    "cig": "`cig run`", "cig-dry": "`cig run --dry-run`", "cig-first": "`cig run`, new folder",
    "stage-a": "stage a", "stage-a-dry": "stage a, `--dry-run`",
    "stage-b": "stage b", "stage-b-dry": "stage b, `--dry-run`",
    "lua": "Lua", "luajit-off": "LuaJIT `-joff`", "luajit": "LuaJIT",
    "python": "CPython", "node": "Node", "rust": "Rust",
    "python-idiom": "CPython, idiomatic", "node-idiom": "Node, idiomatic", "rust-idiom": "Rust, idiomatic",
}
# The versions the conditions table names, and what it calls each.
VERSION_ROWS = [("cig", "cig"), ("stage-a", "cigcompile"), ("lua", "Lua"), ("luajit", "LuaJIT"),
                ("python", "CPython"), ("node", "Node"), ("rust", "rustc")]
LEVEL = 0.03   # a ratio within 3% of 1 is level: neither side is called faster
MOVED = 1.10   # a pair whose per-pass medians differ by more than 10% is listed
DASH = "–"


def fmt_ms(v):
    return "%.1f" % v


def fmt_ratio(v):
    if v < 10:
        return "%.2f" % v
    if v < 100:
        return "%.1f" % v
    return "%.0f" % v


def fmt_mb(kb):
    return "%.1f" % (kb * 1024 / 1e6)


def rank(values, q):
    """The nearest-rank q-quantile: the ceil(q * n)-th smallest value."""
    ordered = sorted(values)
    return ordered[max(1, math.ceil(q * len(ordered))) - 1]


def geomean(ratios):
    return math.exp(math.fsum(math.log(r) for r in ratios) / len(ratios))


def read_tsv(path):
    with open(path) as f:
        rows = [line.rstrip("\n").split("\t") for line in f if line.strip()]
    return rows[0], rows[1:]


class Pass:
    def __init__(self, folder):
        self.folder = folder
        _, rows = read_tsv(os.path.join(folder, "samples.tsv"))
        self.samples = {}
        for row in rows:
            self.samples.setdefault((row[0], row[1]), []).append((int(row[2]), float(row[3])))
        _, rows = read_tsv(os.path.join(folder, "checks.tsv"))
        self.bad = {(r[0], r[1]) for r in rows if r[2] != "ok"}
        _, rows = read_tsv(os.path.join(folder, "memory.tsv"))
        self.memory = {}
        for r in rows:
            self.memory.setdefault((r[0], r[1]), []).append((int(r[3]), int(r[4])))
        _, rows = read_tsv(os.path.join(folder, "rounds.tsv"))
        self.rounds = rows
        self.conditions = []
        with open(os.path.join(folder, "conditions.txt")) as f:
            for line in f:
                if line.strip():
                    self.conditions.append(line.rstrip("\n").split("\t"))

    def condition(self, key):
        for fields in self.conditions:
            if fields[0] == key:
                return fields[1:]
        return None

    def times(self, program, side):
        return [ms for _, ms in self.samples.get((program, side), [])]


class Numbers:
    def __init__(self, passes):
        self.passes = passes
        self.keys = {}
        programs = sorted({p for ps in passes for p, _ in ps.samples})
        self.start = [p for p in START if p in programs]
        self.compute = [p for p in programs if p not in START]
        self.programs = self.compute + self.start
        self.sides = sorted({s for ps in passes for _, s in ps.samples})

    def put(self, key, value, text):
        if key in self.keys:
            raise SystemExit("key made twice: " + key)
        self.keys[key] = {"value": value, "text": text}

    def text(self, key):
        return self.keys[key]["text"] if key in self.keys else DASH

    def valid(self, program, side):
        """Timed in every pass, and never anything but `ok` in any pass's checks."""
        return all(ps.times(program, side) and (program, side) not in ps.bad for ps in self.passes)

    def pooled(self, program, side):
        return [ms for ps in self.passes for ms in ps.times(program, side)]

    def median(self, program, side, k=None):
        if k is None:
            return statistics.median(self.pooled(program, side))
        return statistics.median(self.passes[k].times(program, side))

    def program_set(self, name):
        return self.compute if name == "compute" else self.start

    def compare(self, a, b, set_name, k=None):
        """(geometric mean of a's median over b's, faster, level, slower, programs, the
        (program, ratio) pairs) or None."""
        both = [p for p in self.program_set(set_name) if self.valid(p, a) and self.valid(p, b)]
        if not both:
            return None
        ratios = [self.median(p, a, k) / self.median(p, b, k) for p in both]
        faster = sum(1 for r in ratios if r < 1 - LEVEL)
        slower = sum(1 for r in ratios if r > 1 + LEVEL)
        return (geomean(ratios), faster, len(ratios) - faster - slower, slower, len(ratios),
                list(zip(both, ratios)))

    def build(self):
        np = len(self.passes)
        for p in self.programs:
            for s in self.sides:
                if not self.valid(p, s):
                    continue
                pooled = self.pooled(p, s)
                self.put("median.%s.%s" % (p, s), statistics.median(pooled), fmt_ms(statistics.median(pooled)))
                self.put("p10.%s.%s" % (p, s), rank(pooled, 0.10), fmt_ms(rank(pooled, 0.10)))
                self.put("p90.%s.%s" % (p, s), rank(pooled, 0.90), fmt_ms(rank(pooled, 0.90)))
                for k in range(np):
                    m = self.median(p, s, k)
                    self.put("median.pass%d.%s.%s" % (k + 1, p, s), m, fmt_ms(m))
                readings = [r for ps in self.passes for r in ps.memory.get((p, s), [])]
                if all(ps.memory.get((p, s)) for ps in self.passes):
                    peak = statistics.median([r[0] for r in readings])
                    anon = statistics.median([r[1] for r in readings])
                    self.put("peak.%s.%s" % (p, s), peak, fmt_mb(peak))
                    self.put("anon.%s.%s" % (p, s), anon, fmt_mb(anon))
        pairs = [(a, b) for a in CIG_SIDES for b in LANGUAGES]
        pairs += [("stage-b", "cig"), ("stage-a", "cig"), ("stage-b-dry", "cig-dry")]
        pairs += [(i, base) for i, base in IDIOMS.items()]
        pairs += [("stage-b", i) for i in IDIOMS]
        for a, b in pairs:
            for p in self.programs:
                if self.valid(p, a) and self.valid(p, b):
                    r = self.median(p, a) / self.median(p, b)
                    self.put("ratio.%s.%s.%s" % (p, a, b), r, fmt_ratio(r))
            for set_name in ("compute", "start"):
                got = self.compare(a, b, set_name)
                if got is None:
                    continue
                g, faster, level, slower, n, each = got
                tag = "%s.%s.%s" % (set_name, a, b)
                low = min(each, key=lambda pr: pr[1])
                high = max(each, key=lambda pr: pr[1])
                self.put("lowest.%s" % tag, low[1], low[0])
                self.put("highest.%s" % tag, high[1], high[0])
                self.put("geomean.%s" % tag, g, fmt_ratio(g))
                self.put("inverse.%s" % tag, 1 / g, fmt_ratio(1 / g))
                self.put("faster.%s" % tag, faster, str(faster))
                self.put("level.%s" % tag, level, str(level))
                self.put("slower.%s" % tag, slower, str(slower))
                self.put("n.%s" % tag, n, str(n))
                for k in range(np):
                    gk = self.compare(a, b, set_name, k)[0]
                    self.put("geomean.pass%d.%s" % (k + 1, tag), gk, fmt_ratio(gk))
        # The run record: what a run costs over a dry run of the same program.
        for real in ("cig", "stage-a", "stage-b"):
            dry = real + "-dry"
            both = [p for p in self.programs if self.valid(p, real) and self.valid(p, dry)]
            if not both:
                continue
            extra = statistics.median([self.median(p, real) - self.median(p, dry) for p in both])
            g = geomean([self.median(p, real) / self.median(p, dry) for p in both])
            self.put("record.n.%s" % real, len(both), str(len(both)))
            self.put("record.extra.%s" % real, extra, fmt_ms(extra))
            self.put("record.ratio.%s" % real, g, fmt_ratio(g))
        # Each pass against the first, each side; the drift inside a pass; the noise.
        for s in self.sides:
            progs = [p for p in self.programs if self.valid(p, s)]
            if not progs:
                continue
            self.put("programs.%s" % s, len(progs), str(len(progs)))
            for k in range(1, np):
                g = geomean([self.median(p, s, k) / self.median(p, s, 0) for p in progs])
                self.put("repeat.pass%d.%s" % (k + 1, s), g, fmt_ratio(g))
            for k, ps in enumerate(self.passes):
                rounds = int(ps.condition("rounds")[0])
                third = rounds // 3
                if third == 0:
                    continue
                ratios = []
                for p in progs:
                    by_round = dict(ps.samples[(p, s)])
                    early = [by_round[r] for r in range(1, third + 1) if r in by_round]
                    late = [by_round[r] for r in range(rounds - third + 1, rounds + 1) if r in by_round]
                    ratios.append(statistics.median(late) / statistics.median(early))
                g = geomean(ratios)
                self.put("drift.pass%d.%s" % (k + 1, s), g, fmt_ratio(g))
                noisy = sum(1 for p in progs
                            if statistics.pstdev(ps.times(p, s)) / statistics.fmean(ps.times(p, s)) > 0.05)
                self.put("noisy.pass%d.%s" % (k + 1, s), noisy, str(noisy))
        for k, ps in enumerate(self.passes):
            tag = "pass%d" % (k + 1)
            floor = float(ps.condition("floor_ms")[0])
            self.put("floor.%s" % tag, floor, "%.2f" % floor)
            self.put("rounds.%s" % tag, int(ps.condition("rounds")[0]), ps.condition("rounds")[0])
            self.put("date.%s" % tag, ps.condition("date_utc")[0], ps.condition("date_utc")[0])
            count = sum(len(v) for v in ps.samples.values())
            self.put("samples.%s" % tag, count, str(count))
            self.put("pairs.%s" % tag, len(ps.samples), str(len(ps.samples)))
            self.put("failed.%s" % tag, len(ps.bad), str(len(ps.bad)))
            history = [int(r[4]) for r in ps.rounds]
            mhz = [int(r[2]) for r in ps.rounds if re.fullmatch(r"[0-9]+", r[2])]
            temp = [int(r[3]) for r in ps.rounds if re.fullmatch(r"-?[0-9]+", r[3])]
            if history:
                self.put("history.first.%s" % tag, history[0], str(history[0]))
                self.put("history.last.%s" % tag, history[-1], str(history[-1]))
            if mhz:
                self.put("mhz.min.%s" % tag, min(mhz), str(min(mhz)))
                self.put("mhz.max.%s" % tag, max(mhz), str(max(mhz)))
            if temp:
                self.put("temp.max.%s" % tag, max(temp), str(max(temp)))
        self.put("passes", np, str(np))
        self.put("programs.compute", len(self.compute), str(len(self.compute)))
        self.put("programs.start", len(self.start), str(len(self.start)))
        return self

    # ---- tables -------------------------------------------------------------------

    def grid(self, head, rows, text_columns=1):
        """A Markdown table. The first `text_columns` columns are text, aligned left; the
        rest are numbers, aligned right. A row whose every number is missing is left out."""
        align = ["---"] * text_columns + ["---:"] * (len(head) - text_columns)
        rows = [r for r in rows if text_columns == len(r) or any(c != DASH for c in r[text_columns:])]
        out = ["| " + " | ".join(head) + " |", "|" + "|".join(align) + "|"]
        out += ["| " + " | ".join(r) + " |" for r in rows]
        return "\n".join(out)

    def table_by_program(self, prefix, sides):
        head = ["program"] + [LABEL[s] for s in sides]
        rows = [["`%s`" % p] + [self.text("%s.%s.%s" % (prefix, p, s)) for s in sides] for p in self.programs]
        return self.grid(head, rows)

    def table_spread(self, sides):
        head = ["program"] + [LABEL[s] for s in sides]
        rows = []
        for p in self.programs:
            cells = []
            for s in sides:
                if "p10.%s.%s" % (p, s) in self.keys:
                    cells.append("%s–%s" % (self.text("p10.%s.%s" % (p, s)), self.text("p90.%s.%s" % (p, s))))
                else:
                    cells.append(DASH)
            rows.append(["`%s`" % p] + cells)
        return self.grid(head, rows)

    def table_ratios(self, rows_sides, set_name):
        head = ["time ÷ theirs"] + [LABEL[b] for b in LANGUAGES]
        rows = []
        for a in rows_sides:
            cells = []
            for b in LANGUAGES:
                tag = "%s.%s.%s" % (set_name, a, b)
                if "geomean." + tag in self.keys:
                    cells.append("%s (%s/%s/%s)" % (self.text("geomean." + tag), self.text("faster." + tag),
                                                    self.text("level." + tag), self.text("slower." + tag)))
                else:
                    cells.append(DASH)
            rows.append([LABEL[a]] + cells)
        return self.grid(head, rows)

    def table_record(self):
        head = ["", "programs", "run ÷ dry run", "run − dry run, median ms"]
        rows = []
        for real in ("cig", "stage-a", "stage-b"):
            if "record.n." + real in self.keys:
                rows.append([LABEL[real], self.text("record.n." + real), self.text("record.ratio." + real),
                             self.text("record.extra." + real)])
        return self.grid(head, rows)

    def table_idiom(self):
        head = ["program", "language", "translation, ms", "idiomatic, ms", "idiomatic ÷ translation",
                "stage b ÷ idiomatic"]
        rows = []
        for idiom, base in IDIOMS.items():
            for p in self.programs:
                if self.valid(p, idiom) and self.valid(p, base):
                    rows.append(["`%s`" % p, LABEL[base], self.text("median.%s.%s" % (p, base)),
                                 self.text("median.%s.%s" % (p, idiom)),
                                 self.text("ratio.%s.%s.%s" % (p, idiom, base)),
                                 self.text("ratio.%s.stage-b.%s" % (p, idiom))])
        return self.grid(head, rows, text_columns=2)

    def table_passes(self):
        np = len(self.passes)
        head = ["compute set"]
        for a in ("cig", "stage-b"):
            head += ["%s ÷, pass %d" % (LABEL[a], k + 1) for k in range(np)]
        rows = []
        for b in LANGUAGES:
            cells = []
            for a in ("cig", "stage-b"):
                for k in range(np):
                    cells.append(self.text("geomean.pass%d.compute.%s.%s" % (k + 1, a, b)))
            rows.append([LABEL[b]] + cells)
        return self.grid(head, rows)

    def table_moved(self):
        np = len(self.passes)
        head = ["program", "side"] + ["pass %d, ms" % (k + 1) for k in range(np)]
        rows = []
        for p in self.programs:
            for s in CIG_SIDES + ["cig-first"] + LANGUAGES + list(IDIOMS):
                if not self.valid(p, s):
                    continue
                medians = [self.median(p, s, k) for k in range(np)]
                if max(medians) / min(medians) > MOVED:
                    rows.append(["`%s`" % p, LABEL[s]] + [self.text("median.pass%d.%s.%s" % (k + 1, p, s))
                                                          for k in range(np)])
        return self.grid(head, rows, text_columns=2)

    def table_stability(self):
        np = len(self.passes)
        head = ["side", "programs"]
        head += ["pass %d ÷ pass 1" % (k + 1) for k in range(1, np)]
        head += ["last third ÷ first third, pass %d" % (k + 1) for k in range(np)]
        head += ["CV over 5%%, pass %d" % (k + 1) for k in range(np)]
        rows = []
        for s in CIG_SIDES + ["cig-first"] + LANGUAGES + list(IDIOMS):
            if "programs." + s not in self.keys:
                continue
            row = [LABEL[s], self.text("programs." + s)]
            row += [self.text("repeat.pass%d.%s" % (k + 1, s)) for k in range(1, np)]
            row += [self.text("drift.pass%d.%s" % (k + 1, s)) for k in range(np)]
            row += [self.text("noisy.pass%d.%s" % (k + 1, s)) for k in range(np)]
            rows.append(row)
        return self.grid(head, rows)

    def table_conditions(self):
        np = len(self.passes)
        head = [""] + ["pass %d" % (k + 1) for k in range(np)]
        rows = []
        shown = {"mains": lambda v: {"1": "yes", "0": "no"}.get(v, v),
                 "no_turbo": lambda v: {"0": "on", "1": "off"}.get(v, v),
                 "load_average": lambda v: " ".join(v.split()[:3])}
        for key, label in (("date_utc", "started (UTC)"), ("cpu", "processor"), ("cores", "logical cores"),
                           ("max_mhz", "highest clock, MHz"), ("kernel", "kernel"),
                           ("driver", "frequency driver"), ("governor", "CPU governor"),
                           ("epp", "energy preference"), ("no_turbo", "turbo"), ("profile", "power profile"),
                           ("transparent_hugepages", "transparent huge pages"), ("mains", "on mains power"),
                           ("load_average", "load average at the start"), ("temp_c", "package °C at the start"),
                           ("history", "cig's run records at the start"), ("harness", "harness"),
                           ("rounds", "rounds"), ("seed", "seed of the rounds' order"),
                           ("floor_ms", "`true`, median ms")):
            row = [label]
            for ps in self.passes:
                got = ps.condition(key)
                row.append(shown.get(key, lambda v: v)(got[0]) if got else DASH)
            rows.append(row)
        for key, label in (("history.last", "cig's run records at the last round"),
                           ("mhz.min", "mean clock at a round's start, lowest, MHz"),
                           ("mhz.max", "mean clock at a round's start, highest, MHz"),
                           ("temp.max", "package °C at a round's start, highest")):
            rows.append([label] + [self.text("%s.pass%d" % (key, k + 1)) for k in range(np)])
        for side, label in VERSION_ROWS:
            row = [label]
            for ps in self.passes:
                version = [f for f in ps.conditions if f[0] == "side" and f[1] == side]
                row.append(version[0][3] if version else DASH)
            if any(c != DASH for c in row[1:]):
                rows.append(row)
        for fields in self.passes[0].conditions:
            if fields[0] == "sha256":
                row = ["sha256 of `%s`" % fields[2]]
                for ps in self.passes:
                    same = [f for f in ps.conditions if f[0] == "sha256" and f[2] == fields[2]]
                    row.append("`%s`" % same[0][3] if same else DASH)
                if row not in rows:
                    rows.append(row)
        return self.grid(head, rows, text_columns=np + 1)

    def table(self, name):
        tables = {
            "medians": lambda: self.table_by_program("median", MAIN),
            "medians-cig": lambda: self.table_by_program("median", ["cig", "cig-dry", "stage-a", "stage-a-dry",
                                                                    "stage-b", "stage-b-dry"]),
            "spread": lambda: self.table_spread(MAIN),
            "ratios-compute": lambda: self.table_ratios(["cig", "stage-a", "stage-b"], "compute"),
            "ratios-start": lambda: self.table_ratios(["cig", "stage-a", "stage-b"], "start"),
            "ratios-compute-dry": lambda: self.table_ratios(["cig-dry", "stage-a-dry", "stage-b-dry"],
                                                            "compute"),
            "memory": lambda: self.table_by_program("peak", MAIN),
            "memory-own": lambda: self.table_by_program("anon", MAIN),
            "record": self.table_record,
            "idiom": self.table_idiom,
            "passes": self.table_passes,
            "moved": self.table_moved,
            "stability": self.table_stability,
            "conditions": self.table_conditions,
        }
        if name not in tables:
            raise SystemExit("no table named " + name)
        return tables[name]()

    def render(self, template):
        def one(match):
            key = match.group(1)
            if key.startswith("table:"):
                return self.table(key[len("table:"):])
            if key not in self.keys:
                raise SystemExit("the template asks for %s, which this run has no number for" % key)
            return self.keys[key]["text"]
        return re.sub(r"\{\{([^{}]+)\}\}", one, template)


def main():
    ap = argparse.ArgumentParser(description="The numbers of a compare.py run, and the documents that quote them.")
    ap.add_argument("--pass", dest="passes", action="append", required=True, help="a compare.py output folder")
    ap.add_argument("--numbers", required=True, help="where numbers.json goes")
    ap.add_argument("--render", nargs=2, action="append", default=[], metavar=("TEMPLATE", "OUT"))
    args = ap.parse_args()
    numbers = Numbers([Pass(p) for p in args.passes]).build()
    with open(args.numbers, "w") as f:
        json.dump(numbers.keys, f, indent=1, sort_keys=True)
        f.write("\n")
    for template, out in args.render:
        with open(template) as f:
            text = numbers.render(f.read())
        with open(out, "w") as f:
            f.write(text)
    print("%d numbers from %d pass(es)" % (len(numbers.keys), len(numbers.passes)), file=sys.stderr)


if __name__ == "__main__":
    main()
