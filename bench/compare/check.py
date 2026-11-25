#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
"""Independent checker of a cig benchmark report.

Written from SPEC.md alone. It reads the raw pass folders, computes every
number of the report again, renders every template again, and compares the
result with a numbers.json and with rendered documents byte for byte. It
shares no code with report.py. Exit 0 when nothing differs, 1 when anything
does, 2 on a usage or input error.

    check.py --pass DIR [--pass DIR ...] --numbers OUT.json
             [--render TEMPLATE OUT ...] [--contains FILE SNIPPET ...]
             [--earlier NAME NUMBERS.json ...]
"""
import json
import math
import os
import re
import statistics
import sys

DASH = "–"
DIV = "÷"
MINUS = "−"

START_SET = ["hello", "startup-0", "startup-1", "startup-10"]
CIG_SIDES = ["cig", "stage-a", "stage-b", "stage-c",
             "cig-dry", "stage-a-dry", "stage-b-dry", "stage-c-dry"]
LANGUAGES = ["lua", "luajit-off", "luajit", "python", "node", "rust"]
IDIOMS = [("python-idiom", "python"), ("node-idiom", "node"), ("rust-idiom", "rust")]
IDIOM_SIDES = [i for i, _ in IDIOMS]
ENGINES = ["cig-tree", "cig-vm", "cig-vm-cold"]
# The three `auto` sides: `cig run` on the default (cig-script#259), with the
# module kept (`cig-auto`), on a script's first run (`cig-auto-cold`) and on
# its second (`cig-auto-second`).
AUTO_ENGINES = ["cig-auto", "cig-auto-cold", "cig-auto-second"]
MAIN = ["cig", "stage-b", "stage-c"] + LANGUAGES
ALL = (CIG_SIDES + ["stage-c-off"] + ["cig-first"] + ENGINES + AUTO_ENGINES
       + LANGUAGES + IDIOM_SIDES)

LABELS = {
    "cig": "`cig run`",
    "cig-dry": "`cig run --dry-run`",
    "cig-first": "`cig run`, new folder",
    "cig-tree": "`cig run --engine tree`",
    "cig-vm": "`cig run --engine vm`",
    "cig-vm-cold": "`cig run --engine vm`, empty tin",
    "cig-auto": "`cig run` (default)",
    "cig-auto-cold": "`cig run` (default), first run",
    "cig-auto-second": "`cig run` (default), second run",
    "stage-a": "stage a",
    "stage-a-dry": "stage a, `--dry-run`",
    "stage-b": "stage b",
    "stage-b-dry": "stage b, `--dry-run`",
    "stage-c": "stage c",
    "stage-c-dry": "stage c, `--dry-run`",
    "stage-c-off": "stage c, `CIG_NATIVE=0`",
    "lua": "Lua",
    "luajit-off": "LuaJIT `-joff`",
    "luajit": "LuaJIT",
    "python": "CPython",
    "node": "Node",
    "rust": "Rust",
    "python-idiom": "CPython, idiomatic",
    "node-idiom": "Node, idiomatic",
    "rust-idiom": "Rust, idiomatic",
}


class Fatal(Exception):
    pass


# ---------------------------------------------------------------- input

def read_rows(path, columns, optional=False):
    """Data lines of a tab-separated file as lists of the named columns.

    Blank lines are skipped, the first remaining line is the header, and the
    named columns are found by their header names (extra columns are unused).
    """
    try:
        with open(path, "rb") as fh:
            text = fh.read().decode("utf-8")
    except FileNotFoundError:
        if optional:
            return []
        raise Fatal("cannot read %s: no such file" % path)
    except OSError as exc:
        raise Fatal("cannot read %s: %s" % (path, exc))
    lines = [ln for ln in text.split("\n") if ln != ""]
    if not lines:
        if optional:
            return []
        raise Fatal("%s: no header line" % path)
    head = lines[0].split("\t")
    pos = []
    for c in columns:
        if c not in head:
            raise Fatal("%s: header %r lacks column %r" % (path, lines[0], c))
        pos.append(head.index(c))
    rows = []
    for ln in lines[1:]:
        f = ln.split("\t")
        if len(f) <= max(pos):
            raise Fatal("%s: short line %r" % (path, ln))
        rows.append([f[i] for i in pos])
    return rows


class Pass:
    def __init__(self, folder):
        self.folder = folder
        self.samples = {}      # (program, side) -> [(round, ms)]
        self.sample_lines = 0
        for f in read_rows(os.path.join(folder, "samples.tsv"),
                           ["program", "side", "round", "ms"]):
            self.samples.setdefault((f[0], f[1]), []).append((int(f[2]), float(f[3])))
            self.sample_lines += 1
        self.bad_checks = set()   # pairs with a status other than ok
        for f in read_rows(os.path.join(folder, "checks.tsv"),
                           ["program", "side", "status"]):
            if f[2] != "ok":
                self.bad_checks.add((f[0], f[1]))
        self.peaks = {}        # pair -> [peak_kb of every reading]
        self.anons = {}        # pair -> [anon_kb of every reading]
        for f in read_rows(os.path.join(folder, "memory.tsv"),
                           ["program", "side", "reading", "peak_kb", "anon_kb"]):
            self.peaks.setdefault((f[0], f[1]), []).append(int(f[3]))
            self.anons.setdefault((f[0], f[1]), []).append(int(f[4]))
        self.rounds = read_rows(os.path.join(folder, "rounds.tsv"),
                                ["round", "started_utc", "cpu_mhz", "temp_c", "history"],
                                optional=True)
        # native.tsv is optional: a pass may or may not have one.
        self.has_native_tsv = os.path.exists(os.path.join(folder, "native.tsv"))
        self.native_rows = {}   # program -> (stage, compiled_str, left_str, top_str)
        if self.has_native_tsv:
            for f in read_rows(os.path.join(folder, "native.tsv"),
                               ["program", "stage", "compiled", "left", "top"]):
                self.native_rows[f[0]] = (f[1], f[2], f[3], f[4])
        self.cond = []         # every conditions line, as a list of fields
        try:
            with open(os.path.join(folder, "conditions.txt"), "rb") as fh:
                text = fh.read().decode("utf-8")
        except OSError as exc:
            raise Fatal("cannot read conditions.txt of %s: %s" % (folder, exc))
        for ln in text.split("\n"):
            if ln != "":
                self.cond.append(ln.split("\t"))

    def condition(self, key):
        """Values of the first line whose first field is key, or None."""
        for f in self.cond:
            if f[0] == key:
                return f[1:]
        return None

    def values(self, pair):
        return [ms for _, ms in self.samples[pair]]


# ---------------------------------------------------------------- numbers

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


def fmt_count(n):
    return str(n)


def quantile(pooled, q):
    x = sorted(pooled)
    return x[max(1, math.ceil(q * len(x))) - 1]


def geomean(ratios):
    return math.exp(math.fsum(math.log(r) for r in ratios) / len(ratios))


class Model:
    def __init__(self, passes, earlier=None):
        self.passes = passes
        self.earlier = earlier or {}
        progs = set()
        sides = set()
        for p in passes:
            for prog, side in p.samples:
                progs.add(prog)
                sides.add(side)
        self.start = [p for p in START_SET if p in progs]
        self.compute = sorted(p for p in progs if p not in START_SET)
        self.programs = self.compute + self.start
        self.sides = sides
        self._valid = {}
        self.keys = {}
        self._build_native()
        self.build()

    # ------------------------------------------------- native.tsv modelling

    def _build_native(self):
        """Build native(p) info from the last pass that has a line for p.

        Spec section 1 and 4. `native_info[p]` is a dict with keys
        stage, compiled, left, top: `stage` and `top` are the exact strings
        from the file; `compiled` and `left` are `int` where the field is
        digits only, else the literal string `unknown`.

        Earlier passes that also have a line for p must give the same four
        strings as the last pass, else it is an error.
        """
        self.has_native = any(p.has_native_tsv for p in self.passes)
        self.native_info = {}
        self.native_set = []
        if not self.has_native:
            return
        progs_with_native = set()
        for p in self.passes:
            progs_with_native.update(p.native_rows)
        for prog in progs_with_native:
            indexed = [(i, p.native_rows[prog]) for i, p in enumerate(self.passes)
                       if prog in p.native_rows]
            last_idx, last_row = indexed[-1]
            for i, row in indexed[:-1]:
                if row != last_row:
                    raise Fatal(
                        "program %s has differing native.tsv rows: "
                        "pass %d %r vs pass %d %r"
                        % (prog, i + 1, row, last_idx + 1, last_row)
                    )
            stage, compiled_s, left_s, top_s = last_row
            compiled_v = int(compiled_s) if re.fullmatch(r"[0-9]+", compiled_s) else "unknown"
            left_v = int(left_s) if re.fullmatch(r"[0-9]+", left_s) else "unknown"
            self.native_info[prog] = {
                "stage": stage,
                "compiled": compiled_v,
                "left": left_v,
                "top": top_s,
            }
        self.native_set = [p for p in self.compute
                           if p in self.native_info
                           and self.native_info[p]["stage"] == "c"]

    # ------------------------------------------------- pair/sample helpers

    def valid(self, prog, side):
        pair = (prog, side)
        if pair not in self._valid:
            ok = True
            for p in self.passes:
                if pair not in p.samples or pair in p.bad_checks:
                    ok = False
                    break
            self._valid[pair] = ok
        return self._valid[pair]

    def median(self, prog, side):
        pooled = []
        for p in self.passes:
            pooled.extend(p.values((prog, side)))
        return statistics.median(pooled)

    def median_k(self, k, prog, side):
        return statistics.median(self.passes[k - 1].values((prog, side)))

    def put(self, key, text, value):
        self.keys[key] = (text, value)

    def present(self, group):
        """Sides of the group that appear in at least one pass's samples.tsv."""
        return [s for s in group if s in self.sides]

    def pairs(self):
        out = []
        for a in CIG_SIDES:
            for b in LANGUAGES:
                out.append((a, b))
        out += [("stage-b", "cig"), ("stage-c", "stage-b"), ("stage-c", "cig"),
                ("stage-a", "cig"), ("stage-b-dry", "cig-dry"),
                ("stage-c-dry", "cig-dry")]
        out += [("python-idiom", "python"), ("node-idiom", "node"),
                ("rust-idiom", "rust")]
        out += [("stage-b", "python-idiom"), ("stage-c", "python-idiom"),
                ("stage-b", "node-idiom"), ("stage-c", "node-idiom"),
                ("stage-b", "rust-idiom"), ("stage-c", "rust-idiom")]
        out += [("cig-vm", "cig-tree"), ("cig-vm-cold", "cig-tree"),
                ("cig-vm", "stage-b"), ("cig-vm", "stage-c")]
        out += [("cig-vm", b) for b in LANGUAGES]
        out += [("stage-c", "stage-c-off")]
        # cig-script#259's pairs: the default against the tree-walker (the
        # done-when condition) and against the VM (how much the default costs
        # over asking for the VM directly, with the module kept).
        out += [("cig-auto", "cig-tree"), ("cig-auto-cold", "cig-tree"),
                ("cig-auto", "cig-vm"), ("cig-auto-second", "cig-tree")]
        return out

    def build(self):
        npass = len(self.passes)
        # median and peak
        for prog in self.programs:
            for side in sorted(self.sides):
                if not self.valid(prog, side):
                    continue
                m = self.median(prog, side)
                self.put("median.%s.%s" % (prog, side), fmt_ms(m), m)
                pooled = []
                for p in self.passes:
                    pooled.extend(p.values((prog, side)))
                for q, name in ((0.10, "p10"), (0.90, "p90")):
                    v = quantile(pooled, q)
                    self.put("%s.%s.%s" % (name, prog, side), fmt_ms(v), v)
                for k in range(1, npass + 1):
                    mk = self.median_k(k, prog, side)
                    self.put("median.pass%d.%s.%s" % (k, prog, side), fmt_ms(mk), mk)
                if all(len(p.peaks.get((prog, side), [])) > 0 for p in self.passes):
                    peak = statistics.median([v for p in self.passes
                                              for v in p.peaks[(prog, side)]])
                    self.put("peak.%s.%s" % (prog, side), fmt_mb(peak), peak)
                    anon = statistics.median([v for p in self.passes
                                              for v in p.anons[(prog, side)]])
                    self.put("anon.%s.%s" % (prog, side), fmt_mb(anon), anon)
        # ratios, geometric means, faster/level/slower
        for a, b in self.pairs():
            for prog in self.programs:
                if self.valid(prog, a) and self.valid(prog, b):
                    r = self.median(prog, a) / self.median(prog, b)
                    self.put("ratio.%s.%s.%s" % (prog, a, b), fmt_ratio(r), r)
                    # cig-script#259's "within 10%" gate: a first run on the
                    # default is within 10% of the tree-walker's when the
                    # ratio does not exceed 1.10 (a faster first run passes;
                    # a slower one by more than 10% does not).
                    is_within = 1 if r <= 1.10 else 0
                    self.put("within10.%s.%s.%s" % (prog, a, b),
                             "within 10%" if is_within else "past 10%",
                             is_within)
            sets = [("compute", self.compute), ("start", self.start)]
            if (a, b) == ("stage-c", "stage-b"):
                sets.append(("native", self.native_set))
            for setname, members in sets:
                use = [p for p in members if self.valid(p, a) and self.valid(p, b)]
                if not use:
                    continue
                rs = [self.median(p, a) / self.median(p, b) for p in use]
                g = geomean(rs)
                tag = "%s.%s.%s" % (setname, a, b)
                self.put("geomean." + tag, fmt_ratio(g), g)
                inv = 1 / g
                self.put("inverse." + tag, fmt_ratio(inv), inv)
                fa = sum(1 for r in rs if r < 0.97)
                sl = sum(1 for r in rs if r > 1.03)
                self.put("faster." + tag, fmt_count(fa), fa)
                self.put("level." + tag, fmt_count(len(use) - fa - sl), len(use) - fa - sl)
                self.put("slower." + tag, fmt_count(sl), sl)
                self.put("n." + tag, fmt_count(len(use)), len(use))
                lo = hi = 0
                for i, r in enumerate(rs):
                    if r < rs[lo]:
                        lo = i
                    if r > rs[hi]:
                        hi = i
                self.put("lowest." + tag, use[lo], rs[lo])
                self.put("highest." + tag, use[hi], rs[hi])
                for k in range(1, npass + 1):
                    gk = geomean([self.median_k(k, p, a) / self.median_k(k, p, b)
                                  for p in use])
                    self.put("geomean.pass%d.%s" % (k, tag), fmt_ratio(gk), gk)
        # dry-run records
        for r in ("cig", "stage-a", "stage-b", "stage-c"):
            dry = r + "-dry"
            use = [p for p in self.programs if self.valid(p, r) and self.valid(p, dry)]
            if not use:
                continue
            n = len(use)
            self.put("record.n." + r, fmt_count(n), n)
            g = geomean([self.median(p, r) / self.median(p, dry) for p in use])
            self.put("record.ratio." + r, fmt_ratio(g), g)
            e = statistics.median([self.median(p, r) - self.median(p, dry) for p in use])
            self.put("record.extra." + r, fmt_ms(e), e)
        # per-program native keys
        for prog in self.programs:
            if prog not in self.native_info:
                continue
            info = self.native_info[prog]
            stage = info["stage"]
            compiled = info["compiled"]
            left = info["left"]
            top = info["top"]
            self.put("native.stage.%s" % prog, stage, stage)
            if isinstance(compiled, int):
                self.put("native.compiled.%s" % prog, str(compiled), compiled)
            else:
                self.put("native.compiled.%s" % prog, "unknown", "unknown")
            if isinstance(left, int):
                self.put("native.left.%s" % prog, str(left), left)
            else:
                self.put("native.left.%s" % prog, "unknown", "unknown")
            if isinstance(compiled, int) and isinstance(left, int):
                total = compiled + left
                self.put("native.total.%s" % prog, str(total), total)
            self.put("native.top.%s" % prog, top, top)
            if stage == "b":
                summary = "none"
            elif stage == "c":
                parts = []
                if top == "yes":
                    parts.append("the top level")
                if (isinstance(compiled, int) and isinstance(left, int)
                        and compiled + left > 0):
                    parts.append("%s of %s functions" % (compiled, compiled + left))
                summary = ", ".join(parts) if parts else "unknown"
            else:
                summary = "unknown"
            self.put("native.summary.%s" % prog, summary, summary)
        # per-side figures
        for side in sorted(self.sides):
            use = [p for p in self.programs if self.valid(p, side)]
            if not use:
                continue
            self.put("programs." + side, fmt_count(len(use)), len(use))
            for k in range(2, npass + 1):
                g = geomean([self.median_k(k, p, side) / self.median_k(1, p, side)
                             for p in use])
                self.put("repeat.pass%d.%s" % (k, side), fmt_ratio(g), g)
            for k in range(1, npass + 1):
                rc = self.passes[k - 1].condition("rounds")
                if not rc:
                    continue
                rounds = int(rc[0])
                t = rounds // 3
                if t <= 0:
                    continue
                ratios = []
                noisy = 0
                for p in use:
                    obs = self.passes[k - 1].samples[(p, side)]
                    early = [ms for rd, ms in obs if 1 <= rd <= t]
                    late = [ms for rd, ms in obs if rounds - t + 1 <= rd <= rounds]
                    try:
                        ratios.append(statistics.median(late) / statistics.median(early))
                    except statistics.StatisticsError:
                        raise Fatal("drift.pass%d.%s: program %s has no samples in "
                                    "the early or late third" % (k, side, p))
                    x = [ms for _, ms in obs]
                    if statistics.pstdev(x) / statistics.fmean(x) > 0.05:
                        noisy += 1
                g = geomean(ratios)
                self.put("drift.pass%d.%s" % (k, side), fmt_ratio(g), g)
                self.put("noisy.pass%d.%s" % (k, side), fmt_count(noisy), noisy)
        # run conditions and counts
        for k in range(1, npass + 1):
            ps = self.passes[k - 1]
            fl = ps.condition("floor_ms")
            if fl:
                v = float(fl[0])
                self.put("floor.pass%d" % k, "%.2f" % v, v)
            rd = ps.condition("rounds")
            if rd:
                self.put("rounds.pass%d" % k, rd[0], int(rd[0]))
            dt = ps.condition("date_utc")
            if dt:
                self.put("date.pass%d" % k, dt[0], dt[0])
            self.put("samples.pass%d" % k, fmt_count(ps.sample_lines), ps.sample_lines)
            self.put("pairs.pass%d" % k, fmt_count(len(ps.samples)), len(ps.samples))
            nf = len(ps.bad_checks)
            self.put("failed.pass%d" % k, fmt_count(nf), nf)
            if ps.rounds:
                try:
                    hf = int(ps.rounds[0][4])
                    hl = int(ps.rounds[-1][4])
                except ValueError:
                    raise Fatal("pass %d: history field of rounds.tsv is not an integer: "
                                "%r, %r" % (k, ps.rounds[0][4], ps.rounds[-1][4]))
                self.put("history.first.pass%d" % k, fmt_count(hf), hf)
                self.put("history.last.pass%d" % k, fmt_count(hl), hl)
            mhz = [int(r[2]) for r in ps.rounds if re.fullmatch(r"[0-9]+", r[2])]
            if mhz:
                self.put("mhz.min.pass%d" % k, fmt_count(min(mhz)), min(mhz))
                self.put("mhz.max.pass%d" % k, fmt_count(max(mhz)), max(mhz))
            temps = [int(r[3]) for r in ps.rounds if re.fullmatch(r"-?[0-9]+", r[3])]
            if temps:
                self.put("temp.max.pass%d" % k, fmt_count(max(temps)), max(temps))
        self.put("passes", fmt_count(npass), npass)
        self.put("programs.compute", fmt_count(len(self.compute)), len(self.compute))
        self.put("programs.start", fmt_count(len(self.start)), len(self.start))
        if self.has_native:
            self.put("programs.native", fmt_count(len(self.native_set)),
                     len(self.native_set))

    def text(self, key):
        """Text of a key, or the dash when it does not exist."""
        return self.keys[key][0] if key in self.keys else DASH

    # ------------------------------------------------------------ tables

    @staticmethod
    def render_table(header, numeric, rows):
        """numeric[i] tells whether column i is a number column."""
        out = ["| " + " | ".join(header) + " |",
               "|" + "|".join("---:" if n else "---" for n in numeric) + "|"]
        for row in rows:
            if any(numeric) and all(c == DASH for c, n in zip(row, numeric) if n):
                continue
            out.append("| " + " | ".join(row) + " |")
        return "\n".join(out)

    def table(self, name):
        t = self.text
        if name == "spread":
            main = self.present(MAIN)
            header = ["program"] + [LABELS[s] for s in main]
            rows = []
            for p in self.programs:
                row = ["`%s`" % p]
                for s in main:
                    if "p10.%s.%s" % (p, s) in self.keys:
                        row.append(t("p10.%s.%s" % (p, s)) + DASH + t("p90.%s.%s" % (p, s)))
                    else:
                        row.append(DASH)
                rows.append(row)
            return self.render_table(header, [False] + [True] * len(main), rows)
        if name == "moved":
            n = len(self.passes)
            header = ["program", "side"] + ["pass %d, ms" % k for k in range(1, n + 1)]
            rows = []
            for p in self.programs:
                for s in ALL:
                    if not self.valid(p, s):
                        continue
                    ms = [self.median_k(k, p, s) for k in range(1, n + 1)]
                    if max(ms) / min(ms) > 1.10:
                        rows.append(["`%s`" % p, LABELS[s]]
                                    + [t("median.pass%d.%s.%s" % (k, p, s))
                                       for k in range(1, n + 1)])
            return self.render_table(header, [False, False] + [True] * n, rows)
        if name in ("medians", "memory", "memory-own"):
            what = {"medians": "median", "memory": "peak", "memory-own": "anon"}[name]
            main = self.present(MAIN)
            header = ["program"] + [LABELS[s] for s in main]
            rows = [["`%s`" % p] + [t("%s.%s.%s" % (what, p, s)) for s in main]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * len(main), rows)
        if name == "medians-cig":
            sides = self.present(["cig", "cig-dry", "stage-a", "stage-a-dry",
                                  "stage-b", "stage-b-dry", "stage-c", "stage-c-dry"])
            header = ["program"] + [LABELS[s] for s in sides]
            rows = [["`%s`" % p] + [t("median.%s.%s" % (p, s)) for s in sides]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * len(sides), rows)
        if name in ("ratios-compute", "ratios-start", "ratios-compute-dry"):
            setname = "start" if name == "ratios-start" else "compute"
            if name == "ratios-compute-dry":
                rowsides = self.present(["cig-dry", "stage-a-dry",
                                         "stage-b-dry", "stage-c-dry"])
            else:
                rowsides = self.present(["cig", "stage-a", "stage-b", "stage-c"])
            cols = self.present(LANGUAGES)
            header = ["time " + DIV + " theirs"] + [LABELS[s] for s in cols]
            rows = []
            for a in rowsides:
                row = [LABELS[a]]
                for b in cols:
                    tag = "%s.%s.%s" % (setname, a, b)
                    if "geomean." + tag in self.keys:
                        row.append("%s (%s/%s/%s)" % (t("geomean." + tag), t("faster." + tag),
                                                      t("level." + tag), t("slower." + tag)))
                    else:
                        row.append(DASH)
                rows.append(row)
            return self.render_table(header, [False] + [True] * len(cols), rows)
        if name == "native":
            return self.native_table()
        if name == "record":
            header = ["", "programs", "run " + DIV + " dry run",
                      "run " + MINUS + " dry run, median ms"]
            rows = [[LABELS[r], t("record.n." + r), t("record.ratio." + r),
                     t("record.extra." + r)]
                    for r in ("cig", "stage-a", "stage-b", "stage-c")
                    if "record.n." + r in self.keys]
            return self.render_table(header, [False, True, True, True], rows)
        if name == "idiom":
            header = ["program", "language", "translation, ms", "idiomatic, ms",
                      "idiomatic " + DIV + " translation", "stage b " + DIV + " idiomatic"]
            rows = []
            for i, base in IDIOMS:
                for p in self.programs:
                    if self.valid(p, i) and self.valid(p, base):
                        rows.append(["`%s`" % p, LABELS[base], t("median.%s.%s" % (p, base)),
                                     t("median.%s.%s" % (p, i)),
                                     t("ratio.%s.%s.%s" % (p, i, base)),
                                     t("ratio.%s.stage-b.%s" % (p, i))])
            return self.render_table(header, [False, False, True, True, True, True], rows)
        if name == "passes":
            n = len(self.passes)
            rowsides = self.present(["cig", "stage-b", "stage-c"])
            cols = [(a, k) for a in rowsides for k in range(1, n + 1)]
            langs = self.present(LANGUAGES)
            header = ["compute set"] + ["%s %s, pass %d" % (LABELS[a], DIV, k) for a, k in cols]
            rows = [[LABELS[b]] + [t("geomean.pass%d.compute.%s.%s" % (k, a, b))
                                   for a, k in cols] for b in langs]
            return self.render_table(header, [False] + [True] * len(cols), rows)
        if name == "stability":
            n = len(self.passes)
            header = ["side", "programs"]
            header += ["pass %d %s pass 1" % (k, DIV) for k in range(2, n + 1)]
            header += ["last third %s first third, pass %d" % (DIV, k) for k in range(1, n + 1)]
            header += ["CV over 5%%, pass %d" % k for k in range(1, n + 1)]
            rows = []
            for s in ALL:
                if "programs." + s not in self.keys:
                    continue
                row = [LABELS[s], t("programs." + s)]
                row += [t("repeat.pass%d.%s" % (k, s)) for k in range(2, n + 1)]
                row += [t("drift.pass%d.%s" % (k, s)) for k in range(1, n + 1)]
                row += [t("noisy.pass%d.%s" % (k, s)) for k in range(1, n + 1)]
                rows.append(row)
            return self.render_table(header, [False] + [True] * (len(header) - 1), rows)
        if name == "engines":
            engines = self.present(ENGINES)
            header = ["program"] + [LABELS[s] for s in engines] + [
                "vm " + DIV + " tree", "vm, empty tin " + DIV + " tree"]
            rows = [["`%s`" % p]
                    + [t("median.%s.%s" % (p, s)) for s in engines]
                    + [t("ratio.%s.cig-vm.cig-tree" % p),
                       t("ratio.%s.cig-vm-cold.cig-tree" % p)]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * (len(engines) + 2), rows)
        if name == "engines-auto":
            # Four engines: the tree-walker, the VM (module kept), the default,
            # and the VM with nothing in the tin; then three ratios against the
            # tree-walker. The first-runs doc has its own five-engine table
            # (`engines-first`) with the cold default too.
            cols = ["cig-tree", "cig-vm", "cig-auto", "cig-vm-cold"]
            header = (["program"] + [LABELS[s] for s in cols]
                      + ["vm " + DIV + " tree",
                         "auto " + DIV + " tree",
                         "vm, empty tin " + DIV + " tree"])
            rows = [["`%s`" % p]
                    + [t("median.%s.%s" % (p, s)) for s in cols]
                    + [t("ratio.%s.cig-vm.cig-tree" % p),
                       t("ratio.%s.cig-auto.cig-tree" % p),
                       t("ratio.%s.cig-vm-cold.cig-tree" % p)]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * (len(cols) + 3), rows)
        if name == "engines-first":
            cols = ["cig-tree", "cig-auto-cold", "cig-auto-second", "cig-auto",
                    "cig-vm-cold", "cig-vm"]
            header = ["program"] + [LABELS[s] for s in cols]
            rows = [["`%s`" % p] + [t("median.%s.%s" % (p, s)) for s in cols]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * len(cols), rows)
        if name == "engines-first-ratios":
            cols = ["cig-auto-cold", "cig-auto-second", "cig-auto", "cig-vm-cold", "cig-vm"]
            header = ["program"] + [LABELS[s] for s in cols]
            rows = [["`%s`" % p] + [t("ratio.%s.%s.cig-tree" % (p, s)) for s in cols]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * len(cols), rows)
        if name == "conditions":
            return self.conditions_table()
        if name.startswith("since:"):
            return self.since_table(name[len("since:"):])
        raise Fatal("unknown table %r" % name)

    def native_table(self):
        t = self.text
        header = ["program", "native", "stage b, ms", "stage c, ms",
                  "stage c " + DIV + " stage b"]
        numeric = [False, False, True, True, True]
        rows = []
        for p in self.compute:
            if not (self.valid(p, "stage-b") and self.valid(p, "stage-c")):
                continue
            summary = t("native.summary.%s" % p)
            rows.append(["`%s`" % p, summary,
                         t("median.%s.stage-b" % p),
                         t("median.%s.stage-c" % p),
                         t("ratio.%s.stage-c.stage-b" % p)])
        for setname, label in (("compute", "**all compute programs**"),
                               ("native", "**programs with native code**")):
            key = "geomean.%s.stage-c.stage-b" % setname
            if key not in self.keys:
                continue
            ratio_cell = "%s (%s/%s/%s)" % (
                t("geomean.%s.stage-c.stage-b" % setname),
                t("faster.%s.stage-c.stage-b" % setname),
                t("level.%s.stage-c.stage-b" % setname),
                t("slower.%s.stage-c.stage-b" % setname),
            )
            rows.append([label, DASH,
                         t("n.%s.stage-c.stage-b" % setname), DASH, ratio_cell])
        return self.render_table(header, numeric, rows)

    def since_table(self, name):
        if name not in self.earlier:
            raise Fatal("template names an earlier run that does not exist: %r" % name)
        earlier = self.earlier[name]
        main = self.present(MAIN)
        header = ["program"] + [LABELS[s] for s in main]
        numeric = [False] + [True] * len(main)
        compute_set = set(self.compute)
        column_ratios = [[] for _ in main]
        rows = []
        for p in self.programs:
            row = ["`%s`" % p]
            for col_idx, s in enumerate(main):
                key = "median.%s.%s" % (p, s)
                cell = DASH
                if key in self.keys:
                    entry = earlier.get(key)
                    if (isinstance(entry, dict) and "value" in entry):
                        v = entry["value"]
                        if (not isinstance(v, bool)
                                and isinstance(v, (int, float))
                                and v > 0):
                            r = self.keys[key][1] / v
                            cell = fmt_ratio(r)
                            if p in compute_set:
                                column_ratios[col_idx].append(r)
                row.append(cell)
            rows.append(row)
        summary = ["**compute programs, geometric mean**"]
        for col_idx in range(len(main)):
            if column_ratios[col_idx]:
                summary.append(fmt_ratio(geomean(column_ratios[col_idx])))
            else:
                summary.append(DASH)
        rows.append(summary)
        return self.render_table(header, numeric, rows)

    def conditions_table(self):
        n = len(self.passes)
        header = [""] + ["pass %d" % k for k in range(1, n + 1)]
        rows = []

        def first(ps, key):
            v = ps.condition(key)
            return v[0] if v else None

        simple = [("date_utc", "started (UTC)"), ("cpu", "processor"),
                  ("cores", "logical cores"), ("max_mhz", "highest clock, MHz"),
                  ("kernel", "kernel"), ("driver", "frequency driver"),
                  ("governor", "CPU governor"), ("epp", "energy preference"),
                  ("no_turbo", "turbo"), ("profile", "power profile"),
                  ("transparent_hugepages", "transparent huge pages"),
                  ("mains", "on mains power"), ("load_average", "load average at the start"),
                  ("temp_c", "package °C at the start"),
                  ("history", "cig's run records at the start"),
                  ("harness", "harness"), ("rounds", "rounds"),
                  ("seed", "seed of the rounds' order"),
                  ("cpus", "pinned to CPUs"),
                  ("floor_ms", "`true`, median ms"),
                  ("rest", "rest after each run, times the run's length"),
                  ("warm", "CPU kept busy before each run, ms"),
                  ("cool", "a round waits for the package to be below, °C"),
                  ("hot_rounds", "rounds begun without it"),
                  ("lowering_timeouts", "waits for a left-behind lowering that gave up")]
        for key, label in simple:
            if not any(ps.condition(key) is not None for ps in self.passes):
                continue
            row = [label]
            for ps in self.passes:
                v = first(ps, key)
                if v is None:
                    row.append(DASH)
                elif key == "mains":
                    row.append("yes" if v == "1" else "no" if v == "0" else v)
                elif key == "no_turbo":
                    row.append("on" if v == "0" else "off" if v == "1" else v)
                elif key == "load_average":
                    row.append(" ".join(v.split()[:3]))
                else:
                    row.append(v)
            rows.append(row)
        for label, kname in (("cig's run records at the last round", "history.last"),
                             ("mean clock at a round's start, lowest, MHz", "mhz.min"),
                             ("mean clock at a round's start, highest, MHz", "mhz.max"),
                             ("package °C at a round's start, highest", "temp.max")):
            rows.append([label] + [self.text("%s.pass%d" % (kname, k)) for k in range(1, n + 1)])
        for side, label in (("cig", "cig"), ("stage-a", "cigcompile"), ("lua", "Lua"),
                            ("luajit", "LuaJIT"), ("python", "CPython"), ("node", "Node"),
                            ("rust", "rustc")):
            row = [label]
            found = False
            for ps in self.passes:
                val = DASH
                for f in ps.cond:
                    if f[0] == "side" and len(f) > 1 and f[1] == side:
                        if len(f) > 3:
                            val = f[3]
                            found = True
                        break
                row.append(val)
            if found:
                rows.append(row)
        for f in self.passes[0].cond:
            if f[0] != "sha256" or len(f) < 4:
                continue
            fname = f[2]
            row = ["sha256 of `%s`" % fname]
            for ps in self.passes:
                val = DASH
                for g in ps.cond:
                    if g[0] == "sha256" and len(g) > 3 and g[2] == fname:
                        val = "`%s`" % g[3]
                        break
                row.append(val)
            if row not in rows:
                rows.append(row)
        return self.render_table(header, [False] * len(header), rows)

    def render(self, template):
        def sub(m):
            inner = m.group(1)
            if inner.startswith("table:"):
                return self.table(inner[len("table:"):])
            if inner.startswith("earlier:"):
                rest = inner[len("earlier:"):]
                if ":" not in rest:
                    raise Fatal("template placeholder %r has no second colon" % inner)
                name, key = rest.split(":", 1)
                if name not in self.earlier:
                    raise Fatal("template names an earlier run that does not exist: %r"
                                % name)
                if key not in self.earlier[name]:
                    raise Fatal("template names a key the earlier run %r does not have: %r"
                                % (name, key))
                return self.earlier[name][key]["text"]
            if inner not in self.keys:
                raise Fatal("template names a key that does not exist: %r" % inner)
            return self.keys[inner][0]
        return re.sub(r"\{\{([^{}]+)\}\}", sub, template)


# ---------------------------------------------------------------- compare

def same_value(mine, theirs):
    if isinstance(mine, str):
        return isinstance(theirs, str) and mine == theirs
    if isinstance(theirs, bool) or not isinstance(theirs, (int, float)):
        return False
    if isinstance(mine, int):
        return isinstance(theirs, int) and mine == theirs
    if mine == theirs:
        return True
    return abs(mine - theirs) <= 1e-9 * max(abs(mine), abs(theirs))


def compare_numbers(model, path, diffs):
    try:
        with open(path, "rb") as fh:
            raw = fh.read().decode("utf-8")
        theirs = json.loads(raw)
    except (OSError, ValueError) as exc:
        diffs.append("numbers: cannot read %s: %s" % (path, exc))
        return
    if not isinstance(theirs, dict):
        diffs.append("numbers: top level is not an object")
        return
    if json.dumps(theirs, indent=1, sort_keys=True) + "\n" != raw:
        diffs.append("numbers: %s is not json.dump(indent=1, sort_keys=True) plus a newline"
                     % path)
    for key in sorted(set(model.keys) - set(theirs)):
        diffs.append("key %s: missing from numbers.json (check.py: %r)" % (key, model.keys[key][0]))
    for key in sorted(set(theirs) - set(model.keys)):
        diffs.append("key %s: only in numbers.json (%r)" % (key, theirs[key]))
    for key in sorted(set(model.keys) & set(theirs)):
        text, value = model.keys[key]
        entry = theirs[key]
        if not isinstance(entry, dict) or set(entry) != {"text", "value"}:
            diffs.append("key %s: entry is not {text, value}: %r" % (key, entry))
            continue
        if entry["text"] != text:
            diffs.append("key %s: text check.py %r, numbers.json %r" % (key, text, entry["text"]))
        if not same_value(value, entry["value"]):
            diffs.append("key %s: value check.py %r, numbers.json %r"
                         % (key, value, entry["value"]))


def first_difference(mine, theirs):
    a = mine.split("\n")
    b = theirs.split("\n")
    for i in range(max(len(a), len(b))):
        x = a[i] if i < len(a) else "<end of document>"
        y = b[i] if i < len(b) else "<end of document>"
        if x != y:
            return "line %d: check.py %r, document %r" % (i + 1, x, y)
    return "same lines, different bytes"


def parse_args(argv):
    opts = {"pass": [], "numbers": None, "render": [], "contains": [], "earlier": []}
    known = {"--pass", "--numbers", "--render", "--contains", "--earlier"}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--pass" and i + 1 < len(argv):
            opts["pass"].append(argv[i + 1])
            i += 2
        elif a == "--numbers" and i + 1 < len(argv):
            opts["numbers"] = argv[i + 1]
            i += 2
        elif a in ("--render", "--contains", "--earlier"):
            i += 1
            got = 0
            while i + 1 < len(argv) and (got == 0 or argv[i] not in known):
                opts[a[2:]].append((argv[i], argv[i + 1]))
                i += 2
                got += 1
            if got == 0:
                raise Fatal("%s needs two arguments" % a)
        else:
            raise Fatal("unknown or incomplete argument %r" % a)
    if not opts["pass"] or opts["numbers"] is None:
        raise Fatal("usage: check.py --pass DIR [--pass DIR ...] --numbers OUT.json "
                    "[--render TEMPLATE OUT ...] [--contains FILE SNIPPET ...] "
                    "[--earlier NAME NUMBERS.json ...]")
    return opts


def load_earlier(name, path):
    """Validate and read one --earlier NAME NUMBERS.json.

    Section 9: NAME is not empty and holds no `:`, `{` or `}`. The file is a
    JSON object whose every member is an object with a `text` (string) and a
    `value`. Any failure is a Fatal.
    """
    if name == "":
        raise Fatal("--earlier: NAME is empty")
    for ch in ":{}":
        if ch in name:
            raise Fatal("--earlier: NAME %r contains %r" % (name, ch))
    try:
        with open(path, "rb") as fh:
            raw = fh.read().decode("utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise Fatal("--earlier %s: cannot read %s: %s" % (name, path, exc))
    try:
        obj = json.loads(raw)
    except ValueError as exc:
        raise Fatal("--earlier %s: %s is not JSON: %s" % (name, path, exc))
    if not isinstance(obj, dict):
        raise Fatal("--earlier %s: %s is not a JSON object" % (name, path))
    for k, v in obj.items():
        if (not isinstance(v, dict)
                or set(v) != {"text", "value"}
                or not isinstance(v["text"], str)):
            raise Fatal("--earlier %s: %s member %r is not {text: string, value: ...}"
                        % (name, path, k))
    return obj


def read_text(path):
    with open(path, "rb") as fh:
        return fh.read().decode("utf-8")


def main(argv):
    try:
        opts = parse_args(argv)
        earlier = {}
        for name, path in opts["earlier"]:
            if name in earlier:
                raise Fatal("--earlier: NAME %r given twice" % name)
            earlier[name] = load_earlier(name, path)
        model = Model([Pass(d) for d in opts["pass"]], earlier=earlier)
    except Fatal as exc:
        print("check.py: error: %s" % exc, file=sys.stderr)
        return 2
    if os.environ.get("CHECK_DUMP_KEYS"):
        for key in sorted(model.keys):
            print("KEY %s\t%s\t%r" % (key, model.keys[key][0], model.keys[key][1]))
    diffs = []
    compare_numbers(model, opts["numbers"], diffs)
    docs = 0
    for template, out in opts["render"]:
        docs += 1
        try:
            mine = model.render(read_text(template))
        except Fatal as exc:
            diffs.append("render %s: %s" % (template, exc))
            continue
        except (OSError, UnicodeDecodeError) as exc:
            diffs.append("render %s: cannot read template: %s" % (template, exc))
            continue
        try:
            with open(out, "rb") as fh:
                theirs = fh.read()
        except OSError as exc:
            diffs.append("render %s: cannot read %s: %s" % (template, out, exc))
            continue
        if mine.encode("utf-8") != theirs:
            diffs.append("document %s: %s" % (out, first_difference(
                mine, theirs.decode("utf-8", "replace"))))
    snippets = 0
    for path, snippet in opts["contains"]:
        snippets += 1
        try:
            text = read_text(path)
        except (OSError, UnicodeDecodeError) as exc:
            diffs.append("contains %s: cannot read: %s" % (path, exc))
            continue
        try:
            want = read_text(snippet)
        except (OSError, UnicodeDecodeError) as exc:
            diffs.append("contains %s: cannot read snippet file %s: %s" % (path, snippet, exc))
            continue
        n = text.count(want)
        if n != 1:
            diffs.append("contains %s: snippet %r occurs %d times, expected exactly 1"
                         % (path, want[:80], n))
    for d in diffs:
        print("DIFF " + d)
    print("check.py: %d keys, %d documents checked; differences: %d"
          % (len(model.keys), docs, len(diffs))
          + ("" if not snippets else " (%d snippets checked)" % snippets))
    return 1 if diffs else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
