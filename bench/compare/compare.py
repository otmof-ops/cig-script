#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
"""compare.py: time the programs of bench/compute on cig and on other languages.

    compare.py --out DIR [--rounds N] [--seed N] [--work DIR] [--only NAMES] [--sides NAMES]
               [--cig PATH] [--cigcompile PATH] [--runtime PATH] [--cpus LIST]
               [--lua CMD] [--luajit CMD] [--python CMD] [--node CMD] [--rustc CMD]

What is measured is a whole process, from just before it is started until it
has exited and been reaped: the time a person waits. Nothing is subtracted, an
interpreter's start-up included (`hello` shows what that is for each side).

How a number is made:

1. A side is one way of running a program: `cig run`, a program `cigcompile`
   built, or a reference in `bench/ref/` under its own language. A tool that is
   not installed leaves its sides out, and `conditions.txt` says so.
2. Each (program, side) pair has a folder of its own, at the same path for
   every run, as a user's project has. Its script is copied in once; before
   each run the files a run left there are removed (the folders a run made are
   kept). When that changed anything on disk, the disk is synced before the
   clock starts, so no run pays for the harness's writes; a run's output is
   caught in files in memory (`/dev/shm`), which no sync waits on.
3. Every run is checked. A run counts only if the process exits 0, prints
   exactly the program's `.expect` file, byte for byte, and writes nothing to
   stderr but what its side is known to write there (cig's summary of a run:
   `burned N op(s) (M irreversible), run ID` after a burn, and `dry-run plan:
   0 ops` / `nothing was changed` after a dry run). A pair that does anything
   else, in any round, is WRONG and gets no time at all. Each pair has one
   checked run before the rounds, not timed.
4. In each round every pair runs once, all pairs in an order drawn at random
   from a seeded generator (the seed is in `conditions.txt`), so a machine that
   drifts falls on every program and every side alike, and cig's run history,
   which grows as cig runs and slows only cig's sides, falls on each of their
   programs alike.
5. Every sample is kept (`samples.tsv`, with the CPU time the process and its
   children used). The tables are made from that file by `report.py` and
   recomputed by `check.py`; none is written by hand.
6. After the rounds, each pair's peak resident memory is read three times
   (`memory.tsv`), in a random order.

The wait for a process blocks on its pidfd: Python's `Popen.wait(timeout)`
polls with sleeps that grow from 0.5 to 50 ms, which would add up to 50 ms to
a run. Linux only (the pidfd, and peak memory read from /proc at exit).

`--cpus LIST` pins every measured process (every side, every round, builds
excepted) to those CPUs, in `taskset -c` style (`2`, `0-5`, `0-3,8`). The
affinity is set in the child before exec with `os.sched_setaffinity(0, ...)`,
so nothing of this harness is counted. When the flag is not given nothing is
pinned, and `conditions.txt` says so. The flag is a condition of a pass, not
a number.

DIR gets samples.tsv, checks.tsv, memory.tsv, rounds.tsv, inputs.tsv,
conditions.txt and spec.json.
"""
import argparse
import ctypes
import hashlib
import json
import os
import random
import re
import resource
import select
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.path.dirname(HERE)

# What every cig side runs under: the settings `cigcompile bench` uses. None of
# them makes a run that succeeds do less: the key for sealed secrets is kept in
# a file (a test machine has no keyring), the step budget is off (some programs
# run past its ten million steps), and the two that only speak when a run fails
# (crash reports, the doctor) are quiet.
CIG_ENV = {"CIG_SEAL_KEY": "file", "CIG_MAX_STEPS": "0", "CIG_REPORTS": "never",
           "CIG_DOCTOR": "0", "NO_COLOR": "1"}
# What a cig side may write to stderr, all of it (a regular expression the whole of stderr
# must match): cig's own summary of a run, and nothing else.
CIG_RUN_STDERR = r"(burned [0-9]+ ops? \([0-9]+ irreversible\), run [0-9]{8}T[0-9]{6}-[0-9a-f]+\n)?"
CIG_DRY_STDERR = r"dry-run plan: 0 ops\nnothing was changed\n"
MEMORY_READINGS = 3
TIMEOUT_S = 600

PTRACE_TRACEME, PTRACE_CONT, PTRACE_SETOPTIONS = 0, 7, 0x4200
PTRACE_O_TRACEEXIT, PTRACE_O_EXITKILL, PTRACE_EVENT_EXIT = 0x40, 0x100000, 6


def find(given, names):
    """The tool to use: the one given, else the first of `names` on PATH."""
    if given:
        path = shutil.which(given) or (given if os.path.isfile(given) else None)
        return os.path.abspath(path) if path else None
    for name in names:
        path = shutil.which(name)
        if path:
            return os.path.abspath(path)
    return None


def parse_cpus(spec):
    """A taskset-style list (`2`, `0-5`, `0-3,8`) as a sorted tuple of CPU indices.

    Empty, whitespace or missing parts are errors; so is a CPU not on this host.
    """
    if spec is None:
        return None
    out = set()
    host = os.cpu_count() or 0
    for piece in spec.split(","):
        piece = piece.strip()
        if not piece:
            raise SystemExit("--cpus: empty piece in %r" % spec)
        if "-" in piece:
            a, b = piece.split("-", 1)
            lo, hi = int(a), int(b)
            if lo > hi:
                raise SystemExit("--cpus: range %s has its ends the wrong way" % piece)
            out.update(range(lo, hi + 1))
        else:
            out.add(int(piece))
    for cpu in out:
        if cpu < 0 or cpu >= host:
            raise SystemExit("--cpus: CPU %d is not on this host (0..%d)" % (cpu, host - 1))
    return tuple(sorted(out))


def format_cpus(cpus):
    """The canonical taskset-style text for a tuple of CPU indices (ranges joined by commas)."""
    if not cpus:
        return ""
    parts = []
    cpus = sorted(cpus)
    i = 0
    while i < len(cpus):
        j = i
        while j + 1 < len(cpus) and cpus[j + 1] == cpus[j] + 1:
            j += 1
        parts.append(str(cpus[i]) if j == i else "%d-%d" % (cpus[i], cpus[j]))
        i = j + 1
    return ",".join(parts)


def engines(cig):
    """Whether this cig chooses its engine per run (`cig run --engine`, cig 1.25.0 on)."""
    try:
        p = subprocess.run([cig, "run", "--help"], stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return b"--engine" in p.stdout


def cig_home():
    """The home cig runs with: `CIGSCRIPT_HOME`, else its default."""
    return os.environ.get("CIGSCRIPT_HOME") or os.path.join(os.path.expanduser("~"), ".cigscript")


def build_spec(args):
    compute = os.path.abspath(args.compute)
    ref = os.path.abspath(args.ref)
    programs = []
    burns = []
    for entry in sorted(os.listdir(compute)):
        if not entry.endswith(".cig"):
            continue
        name = entry[:-4]
        expect = os.path.join(compute, name + ".expect")
        if not os.path.isfile(expect):
            continue
        with open(os.path.join(compute, entry)) as f:
            if any(line.lstrip().startswith("burn") for line in f):
                burns.append(name)
        programs.append({"name": name, "expect": expect, "source": os.path.join(compute, entry)})

    script = os.path.join(compute, "{name}.cig")
    sides, missing = [], []

    def side(name, lang, label, **rest):
        sides.append({"name": name, "lang": lang, "label": label, **rest})

    cig = find(args.cig, ["cig"])
    if cig:
        side("cig", "cig", "`cig run`", version=[cig, "--version"], sha256=[cig],
             stage=[script], run=[cig, "run", "{name}.cig"], env=CIG_ENV, stderr=CIG_RUN_STDERR)
        side("cig-dry", "cig", "`cig run --dry-run`", version=[cig, "--version"],
             stage=[script], run=[cig, "run", "--dry-run", "{name}.cig"], env=CIG_ENV, skip=burns,
             stderr=CIG_DRY_STDERR)
        side("cig-first", "cig", "`cig run`, in a new folder each time", version=[cig, "--version"],
             stage=[script], run=[cig, "run", "{name}.cig"], env=CIG_ENV, only=["hello"], fresh=True,
             stderr=CIG_RUN_STDERR)
        # `cig run` under each engine (cig 1.25.0 on). On the VM a script is lowered
        # once and its module kept in the tin: `cig-vm` finds it there (the checked
        # run before the rounds put it there), `cig-vm-cold` never does (its tin's
        # modules are removed before each run, so every run lowers). Each of the
        # three keeps its own home, so one's tin is not another's and all three
        # carry the same number of run records at every round.
        if engines(cig):
            for name, engine, label, cold in (
                    ("cig-tree", "tree", "`cig run --engine tree`", False),
                    ("cig-vm", "vm", "`cig run --engine vm`", False),
                    ("cig-vm-cold", "vm", "`cig run --engine vm`, nothing in the tin", True)):
                home = cig_home() + "-" + name
                side(name, "cig", label, version=[cig, "--version"], stage=[script],
                     run=[cig, "run", "--engine", engine, "{name}.cig"],
                     env=dict(CIG_ENV, CIGSCRIPT_HOME=home), stderr=CIG_RUN_STDERR,
                     cold=os.path.join(home, "tin", "modules") if cold else None)
    else:
        missing.append("cig")
    cigcompile = find(args.cigcompile, ["cigcompile"])
    runtime = find(args.runtime, []) if args.runtime else None
    build_env = dict(CIG_ENV)
    runtime_for_build = runtime or cig
    if runtime_for_build:
        # The runtime a program is built on: the cig being measured by default, so every cig
        # side is one cig. `--runtime` overrides it for all builds and names the one tool.
        build_env["CIGCOMPILE_RUNTIME"] = runtime_for_build
    if cigcompile:
        runtime_flag = ["--runtime", runtime] if runtime else []
        for stage in ("a", "b", "c"):
            program = "{build}/{name}." + stage
            # Stage `c` builds with `--json`, so the build's output lists the stage the program
            # came out as and the native-code counts (`native.tsv`). `--json` is global, so it
            # goes before the subcommand.
            build_it = [cigcompile] + (["--json"] if stage == "c" else []) + \
                ["build", script, "-o", program, "--stage", stage] + runtime_flag
            build_sha = [cigcompile] + ([runtime] if runtime else [])
            side("stage-" + stage, "cig", "a stage `%s` program" % stage,
                 version=[cigcompile, "--version"], sha256=build_sha,
                 prepare=build_it, prepare_env=build_env, creates=program, run=[program],
                 env=CIG_ENV, stderr=CIG_RUN_STDERR,
                 native=(stage == "c"))
            side("stage-%s-dry" % stage, "cig", "a stage `%s` program, `--dry-run`" % stage,
                 version=[cigcompile, "--version"], prepare=build_it, prepare_env=build_env,
                 creates=program, run=[program, "--dry-run"], env=CIG_ENV, skip=burns,
                 stderr=CIG_DRY_STDERR)
        # `stage-c-off` runs the stage-c program with CIG_NATIVE=0 in its environment, so it
        # runs its bytecode and never loads the native code. It is the same built program as
        # `stage-c` (same prepare command, same `creates` path), so one build serves all three
        # stage-c sides: whichever of them runs first builds, and the others find the program
        # on disk and skip the build.
        program_c = "{build}/{name}.c"
        side("stage-c-off", "cig", "a stage `c` program, `CIG_NATIVE=0`",
             version=[cigcompile, "--version"],
             prepare=[cigcompile, "--json", "build", script, "-o", program_c,
                      "--stage", "c"] + runtime_flag,
             prepare_env=build_env, creates=program_c, run=[program_c],
             env=dict(CIG_ENV, CIG_NATIVE="0"), stderr=CIG_RUN_STDERR)
    else:
        missing.append("cigcompile")

    def reference(name, lang, label, tool, ext, how, version, folder=ref):
        """A side that runs the reference programs of one language.

        A script is copied into the pair's folder and run there, as cig's are. A
        Python script is copied as main.py: run under its own name, csv.py and
        json.py would import themselves where they import the csv and json modules.
        Rust is built as `cargo build --release` builds: opt-level 3.
        """
        source = os.path.join(folder, "{name}." + ext)
        if how == "compile":
            program = "{build}/{name}." + name
            side(name, lang, label, version=version, sha256=[tool], needs=source,
                 prepare=[tool, "-C", "opt-level=3", "--edition", "2021", "-o", program, source],
                 creates=program, run=[program])
        else:
            staged = "main.py" if ext == "py" else "{name}." + ext
            side(name, lang, label, version=version, sha256=[tool], needs=source,
                 stage=[[source, staged]], run=[tool] + how + [staged])

    idiom = os.path.join(ref, "idiom")
    lua = find(args.lua, ["lua5.4", "lua"])
    if lua:
        reference("lua", "lua", "Lua", lua, "lua", [], [lua, "-v"])
    else:
        missing.append("lua")
    luajit = find(args.luajit, ["luajit"])
    if luajit:
        reference("luajit-off", "lua", "LuaJIT, its interpreter alone (`-joff`)", luajit, "lua",
                  ["-joff"], [luajit, "-v"])
        reference("luajit", "lua", "LuaJIT", luajit, "lua", [], [luajit, "-v"])
    else:
        missing.append("luajit")
    python = find(args.python, ["python3"])
    if python:
        reference("python", "python", "CPython", python, "py", [], [python, "--version"])
        reference("python-idiom", "python", "CPython, the idiomatic variant", python, "py", [],
                  [python, "--version"], idiom)
    else:
        missing.append("python3")
    node = find(args.node, ["node"])
    if node:
        reference("node", "node", "Node", node, "js", [], [node, "--version"])
        reference("node-idiom", "node", "Node, the idiomatic variant", node, "js", [],
                  [node, "--version"], idiom)
    else:
        missing.append("node")
    rustc = find(args.rustc, ["rustc"])
    if rustc:
        reference("rust", "rust", "Rust (`rustc -C opt-level=3`)", rustc, "rs", "compile",
                  [rustc, "--version"])
        reference("rust-idiom", "rust", "Rust, the idiomatic variant", rustc, "rs", "compile",
                  [rustc, "--version"], idiom)
    else:
        missing.append("rustc")

    if args.only:
        keep = set(args.only.split(","))
        programs = [p for p in programs if p["name"] in keep]
    if args.sides:
        keep = set(args.sides.split(","))
        sides = [s for s in sides if s["name"] in keep]
    return {"rounds": args.rounds, "seed": args.seed, "programs": programs, "sides": sides,
            "not_installed": missing, "compute": compute, "ref": ref}


def fill(parts, name, build):
    return [p.replace("{name}", name).replace("{build}", build) for p in parts]


def wait_for(proc, timeout):
    """Wait for `proc` to exit and reap it; kill it after `timeout` seconds.

    It blocks on the process's pidfd, which is readable the moment the process
    exits, then reaps it: no polling, so no sleep is added to any run.
    """
    try:
        fd = os.pidfd_open(proc.pid)
    except (AttributeError, OSError):
        fd = None
    if fd is not None:
        try:
            poller = select.poll()
            poller.register(fd, select.POLLIN)
            if not poller.poll(timeout * 1000):
                proc.kill()
        finally:
            os.close(fd)
        return proc.wait()
    timer = threading.Timer(timeout, proc.kill)
    timer.start()
    try:
        return proc.wait()
    finally:
        timer.cancel()


class Run:
    """One (program, side) pair: the command, its environment and its folder."""

    def __init__(self, side, prog, work, build, scratch, cpus=None):
        name = prog["name"]
        self.side = side
        self.program = name
        self.key = (name, side["name"])
        self.cmd = fill(side["run"], name, build)
        self.env = dict(os.environ)
        self.env.update(side.get("env") or {})
        # CPUs to pin every measured process to, or None for no pinning. Set in the
        # child before exec, so nothing of this harness is in the measurement.
        self.cpus = cpus
        # What is copied into the folder: a path keeps its file name, a [path, name]
        # pair is copied under that name.
        self.files = []
        for entry in side.get("stage") or []:
            source, as_name = (entry, None) if isinstance(entry, str) else entry
            source = fill([source], name, build)[0]
            as_name = fill([as_name], name, build)[0] if as_name else os.path.basename(source)
            self.files.append((source, as_name))
        self.home = os.path.join(work, "%s.%s" % self.key)
        self.capture = os.path.join(scratch, "%s.%s" % self.key)
        self.fresh = bool(side.get("fresh"))
        self.cold = side.get("cold")
        self.where = self.home
        with open(prog["expect"], "rb") as f:
            self.expect = f.read()

    def ready(self):
        """The folder as the run should find it. Not timed. True when it changed the disk."""
        if self.cold and os.path.isdir(self.cold):
            # A cold run finds no module in its tin: it lowers its own.
            shutil.rmtree(self.cold, ignore_errors=True)
            self.settle()
            return True
        return self.settle()

    def settle(self):
        """`ready`, for the folder itself."""
        if self.fresh:
            if self.where != self.home:
                shutil.rmtree(self.where, ignore_errors=True)
            os.makedirs(self.home, exist_ok=True)
            self.where = tempfile.mkdtemp(dir=self.home)
            for source, as_name in self.files:
                shutil.copy(source, os.path.join(self.where, as_name))
            return True
        changed = not os.path.isdir(self.where)
        os.makedirs(self.where, exist_ok=True)
        own = {as_name for _, as_name in self.files}
        for entry in os.scandir(self.where):
            if entry.name not in own and not entry.is_dir(follow_symlinks=False):
                os.unlink(entry.path)
                changed = True
        for source, as_name in self.files:
            target = os.path.join(self.where, as_name)
            if not os.path.exists(target):
                shutil.copy(source, target)
                changed = True
        return changed

    def once(self):
        """One run: (milliseconds, exit code, stdout bytes, stderr bytes, CPU ms).

        The clock starts just before the process is started and stops when it has
        exited and been reaped. Its output goes to files in memory, opened before the
        clock starts; the disk is synced first when the folder was changed.
        """
        changed = self.ready()
        out_path, err_path = self.capture + ".stdout", self.capture + ".stderr"
        with open(out_path, "wb") as out, open(err_path, "wb") as err:
            if changed:
                os.sync()
            before = resource.getrusage(resource.RUSAGE_CHILDREN)
            pin = (lambda cpus=self.cpus: os.sched_setaffinity(0, cpus)) if self.cpus else None
            start = time.perf_counter()
            try:
                proc = subprocess.Popen(self.cmd, cwd=self.where, env=self.env,
                                        stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                        preexec_fn=pin)
            except OSError as e:
                return 0.0, 127, b"", str(e).encode(), 0.0
            code = wait_for(proc, TIMEOUT_S)
            ms = (time.perf_counter() - start) * 1000.0
            after = resource.getrusage(resource.RUSAGE_CHILDREN)
        cpu = ((after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)) * 1000.0
        with open(out_path, "rb") as f:
            got = f.read()
        with open(err_path, "rb") as f:
            said = f.read()
        return ms, code, got, said, cpu

    def verdict(self, code, got, said):
        """None when a run is right; otherwise what was wrong with it."""
        if code != 0:
            return "exit %d, printed %r, stderr %r" % (code, got[:80], said[:120])
        if got != self.expect:
            return "printed %r" % got[:80]
        if not re.fullmatch(self.side.get("stderr", ""), said.decode("utf-8", "replace")):
            return "stderr %r" % said[:120]
        return None

    def left(self):
        """What the last run left in its folder, beside the pair's own files."""
        own = {as_name for _, as_name in self.files}
        return sorted(e.name + ("/" if e.is_dir() else "") for e in os.scandir(self.where)
                      if e.name not in own)

    def peak_memory(self):
        """One more run's peak resident memory, in kB: (peak, anonymous, file-backed).

        The child asks to be traced and is stopped as it exits, while its address
        space is still there; VmHWM is read from /proc then. That is the peak of
        the program's own address space, with nothing of this harness in it: what
        `wait4` reports for a forked child starts from the parent's own memory.
        None where the system does not let a process be traced by its parent.
        """
        libc = ctypes.CDLL(None, use_errno=True)
        libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]
        libc.ptrace.restype = ctypes.c_long
        self.ready()
        pid = os.fork()
        if pid == 0:
            try:
                if self.cpus:
                    os.sched_setaffinity(0, self.cpus)
                os.chdir(self.where)
                null = os.open(os.devnull, os.O_RDWR)
                for fd in (0, 1, 2):
                    os.dup2(null, fd)
                libc.ptrace(PTRACE_TRACEME, 0, None, None)
                os.execvpe(self.cmd[0], self.cmd, self.env)
            finally:
                os._exit(127)
        seen, traced = None, False
        exit_stop = signal.SIGTRAP | (PTRACE_EVENT_EXIT << 8)
        while True:
            _, status = os.waitpid(pid, 0)
            if not os.WIFSTOPPED(status):
                return seen if os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0 else None
            deliver = 0
            if not traced:
                libc.ptrace(PTRACE_SETOPTIONS, pid, None, PTRACE_O_TRACEEXIT | PTRACE_O_EXITKILL)
                traced = True
            elif status >> 8 == exit_stop:
                try:
                    with open("/proc/%d/status" % pid) as f:
                        fields = dict(line.split(":", 1) for line in f if ":" in line)
                    kb = lambda key: int(fields[key].split()[0])
                    seen = (kb("VmHWM"), kb("RssAnon"), kb("RssFile") + kb("RssShmem"))
                except (OSError, KeyError, ValueError, IndexError):
                    seen = None
            elif os.WSTOPSIG(status) != signal.SIGTRAP:
                deliver = os.WSTOPSIG(status)
            libc.ptrace(PTRACE_CONT, pid, None, deliver)


def first_line(cmd, env):
    try:
        p = subprocess.run(cmd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, timeout=60)
        text = p.stdout.decode(errors="replace").strip().splitlines()
        return text[0].strip() if text else "(no output, exit %d)" % p.returncode
    except (OSError, subprocess.TimeoutExpired) as e:
        return "(could not run: %s)" % e


def read(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return "unknown"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def name_only(value):
    """A path as its file name (`{build}/fib.a` stays as it is): what is published names no
    machine's folders."""
    return os.path.basename(value) if os.path.isabs(value) else value


def cpu_mhz():
    """The mean of every CPU's current frequency, in MHz, or `unknown`."""
    freqs = []
    base = "/sys/devices/system/cpu"
    for entry in sorted(os.listdir(base)):
        if entry.startswith("cpu") and entry[3:].isdigit():
            got = read(os.path.join(base, entry, "cpufreq", "scaling_cur_freq"))
            if got.isdigit():
                freqs.append(int(got))
    return "%.0f" % (statistics.fmean(freqs) / 1000) if freqs else "unknown"


def package_temp():
    """The CPU package's temperature in degrees C, or `unknown`."""
    base = "/sys/class/thermal"
    try:
        zones = sorted(os.listdir(base))
    except OSError:
        return "unknown"
    for zone in zones:
        if read(os.path.join(base, zone, "type")) == "x86_pkg_temp":
            got = read(os.path.join(base, zone, "temp"))
            if got.lstrip("-").isdigit():
                return "%.0f" % (int(got) / 1000)
    return "unknown"


def history():
    """How many run records cig keeps in the home it runs with: the most in any
    one home, where sides keep homes of their own."""
    home = cig_home()
    most = 0
    for h in [home] + [home + "-" + s for s in ("cig-tree", "cig-vm", "cig-vm-cold")]:
        try:
            most = max(most, len(os.listdir(os.path.join(h, "runs"))))
        except OSError:
            pass
    return str(most)


def platform_profile():
    got = read("/sys/firmware/acpi/platform_profile")
    if got != "unknown":
        return got
    try:
        p = subprocess.run(["powerprofilesctl", "get"], stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, timeout=10)
        return p.stdout.decode().strip() or "unknown"
    except (OSError, subprocess.TimeoutExpired):
        return "unknown"


def conditions(spec, out_dir, floor, cpus=None):
    cpu = "unknown"
    for line in read("/proc/cpuinfo").splitlines():
        if line.startswith("model name"):
            cpu = line.split(":", 1)[1].strip()
            break
    cpufreq = "/sys/devices/system/cpu/cpu0/cpufreq"
    max_khz = read(cpufreq + "/cpuinfo_max_freq")
    lines = [
        "date_utc\t" + time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "cpu\t" + cpu,
        "cores\t%d" % os.cpu_count(),
        "kernel\t" + os.uname().release,
        "driver\t" + read(cpufreq + "/scaling_driver"),
        "governor\t" + read(cpufreq + "/scaling_governor"),
        "epp\t" + read(cpufreq + "/energy_performance_preference"),
        "no_turbo\t" + read("/sys/devices/system/cpu/intel_pstate/no_turbo"),
        "max_mhz\t" + ("%.0f" % (int(max_khz) / 1000) if max_khz.isdigit() else "unknown"),
        "profile\t" + platform_profile(),
        "transparent_hugepages\t" + read("/sys/kernel/mm/transparent_hugepage/enabled"),
        "mains\t" + read("/sys/class/power_supply/AC/online"),
        "load_average\t" + read("/proc/loadavg"),
        "temp_c\t" + package_temp(),
        "history\t" + history(),
        "harness\tPython " + sys.version.split()[0],
        "rounds\t%d" % spec["rounds"],
        "seed\t%d" % spec["seed"],
        "cpus\t" + (format_cpus(cpus) if cpus else "(not pinned)"),
        "floor_ms\t%.3f\t%.3f\t%.3f" % floor,
    ]
    for tool in spec.get("not_installed") or []:
        lines.append("not_installed\t" + tool)
    for side in spec["sides"]:
        env = dict(os.environ)
        env.update(side.get("env") or {})
        lines.append("side\t%s\t%s\t%s" % (side["name"], side["label"], first_line(side["version"], env)))
        lines.append("command\t%s\t%s" % (side["name"], " ".join(map(name_only, side["run"]))))
        if side.get("prepare"):
            lines.append("built_by\t%s\t%s" % (side["name"], " ".join(map(name_only, side["prepare"]))))
        for key, value in sorted((side.get("env") or {}).items()):
            lines.append("env\t%s\t%s=%s" % (side["name"], key, name_only(value)))
        for key, value in sorted((side.get("prepare_env") or {}).items()):
            lines.append("built_with\t%s\t%s=%s" % (side["name"], key, name_only(value)))
        for path in side.get("sha256") or []:
            if os.path.isfile(path):
                lines.append("sha256\t%s\t%s\t%s" % (side["name"], os.path.basename(path),
                                                     sha256(os.path.realpath(path))))
    with open(os.path.join(out_dir, "conditions.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")


def measure_floor(work, build, scratch, cpus=None):
    """What starting, waiting for and reaping a process costs here: `true`, 50 times,
    run exactly as every side is (same CPU pinning if any)."""
    true = shutil.which("true")
    if not true:
        return (0.0, 0.0, 0.0)
    expect = os.path.join(work, "floor.expect")
    open(expect, "wb").close()
    run = Run({"name": "floor", "run": [true]}, {"name": "floor", "expect": expect},
              work, build, scratch, cpus=cpus)
    times = [run.once()[0] for _ in range(50)]
    return (statistics.median(times), min(times), max(times))


def main():
    ap = argparse.ArgumentParser(description="Time bench/compute on cig and on other languages.")
    ap.add_argument("--out", required=True, help="the folder the results go in")
    ap.add_argument("--work", help="the folder the programs run in (default: OUT/work)")
    ap.add_argument("--scratch", help="where runs' output is caught (default: a new folder in /dev/shm)")
    ap.add_argument("--rounds", type=int, default=21)
    ap.add_argument("--seed", type=int, default=1, help="the seed of each round's order")
    ap.add_argument("--only", help="programs to run, comma-separated")
    ap.add_argument("--sides", help="sides to run, comma-separated")
    ap.add_argument("--compute", default=os.path.join(BENCH, "compute"))
    ap.add_argument("--ref", default=os.path.join(BENCH, "ref"))
    for tool in ("cig", "cigcompile", "lua", "luajit", "python", "node", "rustc"):
        ap.add_argument("--" + tool, help="the %s to use (default: the one on PATH)" % tool)
    ap.add_argument("--runtime", help="the cig a program is built on (also sets CIGCOMPILE_RUNTIME "
                    "for every `cigcompile build`); default: the cig named by --cig")
    ap.add_argument("--cpus", help="pin every measured process to this taskset-style CPU list "
                    "(for example `2` or `0-5`); builds are not pinned. Default: not pinned.")
    args = ap.parse_args()
    cpus = parse_cpus(args.cpus)

    out_dir = os.path.abspath(args.out)
    work = os.path.abspath(args.work) if args.work else os.path.join(out_dir, "work")
    build = os.path.join(work, "build")
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(build, exist_ok=True)
    if args.scratch:
        scratch = os.path.abspath(args.scratch)
        os.makedirs(scratch, exist_ok=True)
    elif os.access("/dev/shm", os.W_OK):
        scratch = tempfile.mkdtemp(prefix="compare-", dir="/dev/shm")
    else:
        scratch = tempfile.mkdtemp(prefix="compare-", dir=work)
    spec = build_spec(args)
    rounds = int(spec["rounds"])
    with open(os.path.join(out_dir, "spec.json"), "w") as f:
        json.dump(spec, f, indent=1)
    conditions(spec, out_dir, measure_floor(work, build, scratch, cpus=cpus), cpus=cpus)

    samples = open(os.path.join(out_dir, "samples.tsv"), "w")
    checks = open(os.path.join(out_dir, "checks.tsv"), "w")
    memory = open(os.path.join(out_dir, "memory.tsv"), "w")
    round_log = open(os.path.join(out_dir, "rounds.tsv"), "w")
    inputs = open(os.path.join(out_dir, "inputs.tsv"), "w")
    native_tsv = open(os.path.join(out_dir, "native.tsv"), "w")
    samples.write("program\tside\tround\tms\tcpu_ms\n")
    checks.write("program\tside\tstatus\tdetail\n")
    memory.write("program\tside\treading\tpeak_kb\tanon_kb\tfile_kb\n")
    round_log.write("round\tstarted_utc\tcpu_mhz\ttemp_c\thistory\n")
    inputs.write("kind\tname\tsha256\n")
    native_tsv.write("program\tstage\tcompiled\tleft\ttop\n")
    native_written = 0
    failed = 0
    hashed = set()

    def record_input(kind, path):
        if kind == "built":
            name = os.path.basename(path)
        elif kind == "reference":
            name = "ref/" + os.path.relpath(path, spec["ref"])
        else:
            name = "compute/" + os.path.basename(path)
        if (kind, name) not in hashed:
            hashed.add((kind, name))
            inputs.write("%s\t%s\t%s\n" % (kind, name, sha256(path)))

    # Build, then one checked run of every pair, in the programs' order.
    live = []
    for prog in spec["programs"]:
        name = prog["name"]
        record_input("program", prog["source"])
        record_input("expect", prog["expect"])
        for side in spec["sides"]:
            if name in (side.get("skip") or []) or (side.get("only") and name not in side["only"]):
                continue
            needs = side.get("needs")
            if needs and not os.path.exists(needs.replace("{name}", name)):
                continue
            if needs:
                record_input("reference", needs.replace("{name}", name))
            run = Run(side, prog, work, build, scratch, cpus=cpus)
            creates = fill([side["creates"]], name, build)[0] if side.get("creates") else None
            if side.get("prepare") and not (creates and os.path.exists(creates)):
                os.makedirs(run.where, exist_ok=True)
                env = dict(os.environ)
                env.update(side.get("prepare_env") or side.get("env") or {})
                want_json = bool(side.get("native"))
                p = subprocess.run(fill(side["prepare"], name, build), cwd=run.where, env=env,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE if want_json else subprocess.STDOUT)
                if p.returncode != 0:
                    out = p.stdout + (p.stderr or b"") if want_json else p.stdout
                    tail = (out.decode(errors="replace").strip().splitlines() or [""])[-1]
                    checks.write("%s\t%s\tfailed\tbuild exit %d: %s\n"
                                 % (name, side["name"], p.returncode, tail[:200]))
                    print("FAILED %s %s: build exit %d: %s" % (name, side["name"], p.returncode, tail[:200]),
                          flush=True)
                    failed += 1
                    continue
                if want_json:
                    # `cigcompile --json build` prints one JSON object a script. Its `stage`
                    # is `a`, `b` or `c`; its `native`, when the build has native code, is a
                    # map with `compiled` and `left` (the script's functions with native code
                    # and without) and `top` (whether the top level has native code: the top
                    # level is no function to those two counts). A `--stage c` build without
                    # native code exits 0, comes out as `b` and has `native: null` and a
                    # stage-b warning.
                    stage_out, compiled, left, top = "unknown", "unknown", "unknown", "unknown"
                    for line in p.stdout.decode(errors="replace").splitlines():
                        line = line.strip()
                        if not line.startswith("{"):
                            continue
                        try:
                            obj = json.loads(line)
                        except ValueError:
                            continue
                        if obj.get("name") != name and "stage" not in obj:
                            continue
                        stage_out = obj.get("stage") or "unknown"
                        native = obj.get("native") or {}
                        if isinstance(native, dict):
                            compiled = str(native.get("compiled", "unknown"))
                            left = str(native.get("left", "unknown"))
                            if isinstance(native.get("top"), bool):
                                top = "yes" if native["top"] else "no"
                        break
                    native_tsv.write("%s\t%s\t%s\t%s\t%s\n" % (name, stage_out, compiled, left, top))
                    native_tsv.flush()
                    native_written += 1
            if creates:
                record_input("built", creates)
            _, code, got, said, _ = run.once()
            wrong = run.verdict(code, got, said)
            if wrong:
                checks.write("%s\t%s\tWRONG\t%s\n" % (name, side["name"], wrong))
                print("WRONG %s %s: %s" % (name, side["name"], wrong), flush=True)
                failed += 1
                continue
            checks.write("%s\t%s\tok\t%s\n" % (name, side["name"], " ".join(run.left())))
            live.append(run)
    checks.flush()
    inputs.close()
    print("%d pairs ready; %d rounds" % (len(live), rounds), flush=True)

    rng = random.Random(spec["seed"])
    wrong = set()
    started = time.monotonic()
    for r in range(rounds):
        order = live[:]
        rng.shuffle(order)
        round_log.write("%d\t%s\t%s\t%s\t%s\n" % (r + 1, time.strftime("%H:%M:%S", time.gmtime()),
                                                  cpu_mhz(), package_temp(), history()))
        round_log.flush()
        for run in order:
            if run.key in wrong:
                continue
            ms, code, got, said, cpu = run.once()
            bad = run.verdict(code, got, said)
            if bad:
                wrong.add(run.key)
                failed += 1
                checks.write("%s\t%s\tWRONG\tround %d: %s\n" % (run.key + (r + 1, bad)))
                checks.flush()
                print("WRONG %s %s in round %d: %s" % (run.key + (r + 1, bad)), flush=True)
                continue
            samples.write("%s\t%s\t%d\t%.3f\t%.3f\n" % (run.key + (r + 1, ms, cpu)))
        samples.flush()
        print("round %d of %d done, %.0f s" % (r + 1, rounds, time.monotonic() - started), flush=True)

    readings = 0
    for i in range(MEMORY_READINGS):
        order = [run for run in live if run.key not in wrong]
        rng.shuffle(order)
        for run in order:
            m = run.peak_memory()
            if m:
                readings += 1
                memory.write("%s\t%s\t%d\t%d\t%d\t%d\n" % (run.key + (i + 1,) + m))
        memory.flush()
    for f in (samples, checks, memory, round_log):
        f.close()
    native_tsv.close()
    if native_written == 0:
        # A run with no stage `c` builds leaves no `native.tsv`, so a pass that never saw
        # stage `c` is the same on disk as it was before stage `c` was a side.
        try:
            os.unlink(os.path.join(out_dir, "native.tsv"))
        except OSError:
            pass
    shutil.rmtree(work, ignore_errors=True)
    shutil.rmtree(scratch, ignore_errors=True)
    if live and not readings:
        print("NO MEMORY READING could be made: this system does not let a process be traced "
              "by its parent", flush=True)
        failed += 1
    if failed:
        print("%d pair(s) failed or printed the wrong output: see checks.tsv" % failed, flush=True)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
