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
CIG_SIDES = ["cig", "stage-a", "stage-b", "cig-dry", "stage-a-dry", "stage-b-dry"]
LANGUAGES = ["lua", "luajit-off", "luajit", "python", "node", "rust"]
IDIOMS = [("python-idiom", "python"), ("node-idiom", "node"), ("rust-idiom", "rust")]
IDIOM_SIDES = [i for i, _ in IDIOMS]
MAIN = ["cig", "stage-b"] + LANGUAGES
ALL = CIG_SIDES + ["cig-first"] + LANGUAGES + IDIOM_SIDES

LABELS = {
    "cig": "`cig run`",
    "cig-dry": "`cig run --dry-run`",
    "cig-first": "`cig run`, new folder",
    "stage-a": "stage a",
    "stage-a-dry": "stage a, `--dry-run`",
    "stage-b": "stage b",
    "stage-b-dry": "stage b, `--dry-run`",
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
    def __init__(self, passes):
        self.passes = passes
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
        self.build()

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

    def pairs(self):
        out = []
        for a in CIG_SIDES:
            for b in LANGUAGES:
                out.append((a, b))
        out += [("stage-b", "cig"), ("stage-a", "cig"), ("stage-b-dry", "cig-dry")]
        out += [("python-idiom", "python"), ("node-idiom", "node"), ("rust-idiom", "rust")]
        out += [("stage-b", "python-idiom"), ("stage-b", "node-idiom"), ("stage-b", "rust-idiom")]
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
            for setname, members in (("compute", self.compute), ("start", self.start)):
                use = [p for p in members if self.valid(p, a) and self.valid(p, b)]
                if not use:
                    continue
                g = geomean([self.median(p, a) / self.median(p, b) for p in use])
                tag = "%s.%s.%s" % (setname, a, b)
                self.put("geomean." + tag, fmt_ratio(g), g)
                inv = 1 / g
                self.put("inverse." + tag, fmt_ratio(inv), inv)
                rs = [self.median(p, a) / self.median(p, b) for p in use]
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
        for r in ("cig", "stage-a", "stage-b"):
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
            header = ["program"] + [LABELS[s] for s in MAIN]
            rows = []
            for p in self.programs:
                row = ["`%s`" % p]
                for s in MAIN:
                    if "p10.%s.%s" % (p, s) in self.keys:
                        row.append(t("p10.%s.%s" % (p, s)) + DASH + t("p90.%s.%s" % (p, s)))
                    else:
                        row.append(DASH)
                rows.append(row)
            return self.render_table(header, [False] + [True] * len(MAIN), rows)
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
            header = ["program"] + [LABELS[s] for s in MAIN]
            rows = [["`%s`" % p] + [t("%s.%s.%s" % (what, p, s)) for s in MAIN]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * len(MAIN), rows)
        if name == "medians-cig":
            sides = ["cig", "cig-dry", "stage-a", "stage-a-dry", "stage-b", "stage-b-dry"]
            header = ["program"] + [LABELS[s] for s in sides]
            rows = [["`%s`" % p] + [t("median.%s.%s" % (p, s)) for s in sides]
                    for p in self.programs]
            return self.render_table(header, [False] + [True] * len(sides), rows)
        if name in ("ratios-compute", "ratios-start", "ratios-compute-dry"):
            setname = "start" if name == "ratios-start" else "compute"
            rowsides = (["cig-dry", "stage-a-dry", "stage-b-dry"]
                        if name == "ratios-compute-dry" else ["cig", "stage-a", "stage-b"])
            header = ["time " + DIV + " theirs"] + [LABELS[s] for s in LANGUAGES]
            rows = []
            for a in rowsides:
                row = [LABELS[a]]
                for b in LANGUAGES:
                    tag = "%s.%s.%s" % (setname, a, b)
                    if "geomean." + tag in self.keys:
                        row.append("%s (%s/%s/%s)" % (t("geomean." + tag), t("faster." + tag),
                                                      t("level." + tag), t("slower." + tag)))
                    else:
                        row.append(DASH)
                rows.append(row)
            return self.render_table(header, [False] + [True] * len(LANGUAGES), rows)
        if name == "record":
            header = ["", "programs", "run " + DIV + " dry run",
                      "run " + MINUS + " dry run, median ms"]
            rows = [[LABELS[r], t("record.n." + r), t("record.ratio." + r),
                     t("record.extra." + r)]
                    for r in ("cig", "stage-a", "stage-b") if "record.n." + r in self.keys]
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
            cols = [(a, k) for a in ("cig", "stage-b") for k in range(1, n + 1)]
            header = ["compute set"] + ["%s %s, pass %d" % (LABELS[a], DIV, k) for a, k in cols]
            rows = [[LABELS[b]] + [t("geomean.pass%d.compute.%s.%s" % (k, a, b))
                                   for a, k in cols] for b in LANGUAGES]
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
        if name == "conditions":
            return self.conditions_table()
        raise Fatal("unknown table %r" % name)

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
                  ("temp_c", "package \u00b0C at the start"),
                  ("history", "cig's run records at the start"),
                  ("harness", "harness"), ("rounds", "rounds"),
                  ("seed", "seed of the rounds' order"),
                  ("floor_ms", "`true`, median ms")]
        for key, label in simple:
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
                             ("package \u00b0C at a round's start, highest", "temp.max")):
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
    opts = {"pass": [], "numbers": None, "render": [], "contains": []}
    known = {"--pass", "--numbers", "--render", "--contains"}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--pass" and i + 1 < len(argv):
            opts["pass"].append(argv[i + 1])
            i += 2
        elif a == "--numbers" and i + 1 < len(argv):
            opts["numbers"] = argv[i + 1]
            i += 2
        elif a in ("--render", "--contains"):
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
                    "[--render TEMPLATE OUT ...] [--contains FILE SNIPPET ...]")
    return opts


def read_text(path):
    with open(path, "rb") as fh:
        return fh.read().decode("utf-8")


def main(argv):
    try:
        opts = parse_args(argv)
        model = Model([Pass(d) for d in opts["pass"]])
    except Fatal as exc:
        print("check.py: error: %s" % exc, file=sys.stderr)
        return 2
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
