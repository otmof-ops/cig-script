#!/usr/bin/env bash
# Portability probe: the kernel's promises, one scenario each, on whatever OS
# this runs on. Prints PASS/FAIL/SKIP lines; never stops early.
set -u
CIG="${CIG:-cig}"
case "$CIG" in */*) PATH="$(cd "$(dirname "$CIG")" && pwd):$PATH"; export PATH;; esac
ROOT="$(mktemp -d 2>/dev/null || mktemp -d -t cigprobe)"
export CIGSCRIPT_HOME="$ROOT/home" NO_COLOR=1 CIG_SEAL_KEY=file
pass=0; fail=0; skip=0
OS="$(uname -s 2>/dev/null || echo unknown)"
case "$OS" in MINGW*|MSYS*|CYGWIN*) WIN=1 ;; *) WIN=0 ;; esac

result() { # name status detail
  printf '%-4s %-46s %s\n' "$2" "$1" "${3:-}"
  case "$2" in PASS) pass=$((pass+1));; FAIL) fail=$((fail+1));; SKIP) skip=$((skip+1));; esac
}
fresh() { D="$ROOT/s$RANDOM$RANDOM"; mkdir -p "$D" "$D-home"; export CIGSCRIPT_HOME="$D-home"; cd "$D" || exit 1; }
run() { "$CIG" "$@" >out.txt 2>&1; echo $? >code.txt; }
code() { cat code.txt; }
last() { tr '\r\n' '  ' <out.txt | cut -c1-160; }

echo "== probe on $OS ($(uname -m 2>/dev/null)), $("$CIG" --version 2>&1)"
PY=$(command -v python3 || command -v python); PY=$(basename "$PY" .exe)

# 1 dry run changes nothing
fresh; printf 'burn {\n  fs.write_text("a.txt", "1")\n}\n' > s.cig
run run --dry-run s.cig
[ "$(code)" = 0 ] && [ ! -e a.txt ] && result "dry run writes nothing" PASS || result "dry run writes nothing" FAIL "$(last)"

# 2 a failing burn rolls back a create
fresh; printf 'burn {\n  fs.write_text("a.txt", "1")\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ "$(code)" != 0 ] && [ ! -e a.txt ] && result "failed burn removes a created file" PASS || result "failed burn removes a created file" FAIL "$(last)"

# 3 a failing burn restores a modified file
fresh; echo original > keep.txt; printf 'burn {\n  fs.write_text("keep.txt", "changed")\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ "$(tr -d '\r' < keep.txt)" = original ] && grep -q E600 out.txt && result "failed burn restores a modified file" PASS || result "failed burn restores a modified file" FAIL "keep.txt=$(cat keep.txt) $(last)"

# 3b changing an existing file succeeds (the success path, not only rollback)
fresh; echo original > keep.txt; printf 'burn {\n  fs.write_text("keep.txt", "changed")\n}\n' > s.cig
run run s.cig
[ "$(code)" = 0 ] && [ "$(tr -d '\r' < keep.txt)" = changed ] && result "a burn can change an existing file" PASS || result "a burn can change an existing file" FAIL "$(last)"

# 3c deleting an existing file and tree succeeds
fresh; echo x > f.txt; mkdir -p t/u; echo y > t/u/g.txt; printf 'burn {\n  fs.rm("f.txt")\n  fs.rm("t")\n}\n' > s.cig
run run s.cig
[ "$(code)" = 0 ] && [ ! -e f.txt ] && [ ! -e t ] && result "a burn can delete an existing file and tree" PASS || result "a burn can delete an existing file and tree" FAIL "$(last)"

# 4 a failing burn restores a deleted directory tree
fresh; mkdir -p d/e; echo x > d/e/f.txt; printf 'burn {\n  fs.rm("d")\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ -f d/e/f.txt ] && grep -q E600 out.txt && result "failed burn restores a deleted tree" PASS || result "failed burn restores a deleted tree" FAIL "$(last)"

# 5 a move is undone
fresh; echo m > from.txt; printf 'burn {\n  fs.mkdir("to")\n  fs.mv("from.txt", "to/from.txt")\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ -f from.txt ] && [ ! -e to ] && grep -q E600 out.txt && result "failed burn undoes a move and its mkdir" PASS || result "failed burn undoes a move and its mkdir" FAIL "$(ls) $(last)"

# 6 unburn after success
fresh; echo original > keep.txt; printf 'burn {\n  fs.write_text("keep.txt", "changed")\n  fs.write_text("new.txt", "n")\n}\n' > s.cig
run run s.cig; first=$(code); mid=$(tr -d '\r' < keep.txt); id=$(ls "$CIGSCRIPT_HOME/runs" 2>/dev/null | tail -1)
run unburn "$id"
[ "$first" = 0 ] && [ "$mid" = changed ] && [ "$(tr -d '\r' < keep.txt)" = original ] && [ ! -e new.txt ] && result "unburn puts a successful run back" PASS || result "unburn puts a successful run back" FAIL "$(last)"

# 7 unburn refuses after a later edit (E704)
fresh; printf 'burn {\n  fs.write_text("a.txt", "1")\n}\n' > s.cig
run run s.cig; id=$(ls "$CIGSCRIPT_HOME/runs" | tail -1); echo mine >> a.txt
run unburn "$id"
[ "$(code)" != 0 ] && grep -q E704 out.txt && [ -e a.txt ] && result "unburn refuses after a later edit (E704)" PASS || result "unburn refuses after a later edit (E704)" FAIL "$(last)"

# 8 hop watching: a child's created file is rolled back
fresh; printf 'pack { "." }\nburn {\n  proc.run("git", ["init", "-q", "sub"])\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ ! -e sub ] && result "hop-created files in the pack roll back" PASS || result "hop-created files in the pack roll back" FAIL "$(ls) $(last)"

# 9 proc.run finds a program on PATH
fresh; printf 'burn {\n  stick r = proc.run("git", ["--version"])\n  exhale r.out.trim()\n}\n' > s.cig
run run s.cig
grep -q "git version" out.txt && result "proc.run finds git on PATH" PASS || result "proc.run finds git on PATH" FAIL "$(last)"

# 10 proc.run finds a .cmd shim (npm on Windows is npm.cmd)
fresh; printf 'burn {\n  stick r = proc.run("npm", ["--version"])\n  exhale r.code\n}\n' > s.cig
if command -v npm >/dev/null 2>&1; then run run s.cig
  [ "$(code)" = 0 ] && result "proc.run finds npm (a .cmd shim on Windows)" PASS || result "proc.run finds npm (a .cmd shim on Windows)" FAIL "$(last)"
else result "proc.run finds npm (a .cmd shim on Windows)" SKIP "no npm"; fi

# 11 proc.shell works (it uses sh -c)
fresh; printf 'stick r = proc.shell("echo hi")\nexhale r.out.trim()\n' > s.cig
run run s.cig
grep -q hi out.txt && result "proc.shell runs a command line" PASS || result "proc.shell runs a command line" FAIL "$(last)"

# 12 a script saved with CRLF line endings parses
fresh; printf 'stick x = 1\r\nexhale "crlf ok ${x}"\r\n' > s.cig
run run s.cig
grep -q "crlf ok 1" out.txt && result "a CRLF script parses" PASS || result "a CRLF script parses" FAIL "$(last)"

# 13 unicode filename round trip
fresh; printf 'burn {\n  fs.write_text("caf\xc3\xa9-\xe6\x97\xa5\xe6\x9c\xac.txt", "u")\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ -z "$(ls | grep -v -e s.cig -e out.txt -e code.txt)" ] && result "unicode filename rolls back" PASS || result "unicode filename rolls back" FAIL "$(ls) $(last)"

# 14 case-only collision on a case-insensitive filesystem
fresh; echo lower > a.txt; printf 'burn {\n  fs.write_text("A.txt", "upper")\n  cough "boom"\n}\n' > s.cig
run run s.cig
if [ -e A.txt ] && [ -e a.txt ] && [ "$(ls | grep -ci '^a.txt$')" = 1 ]; then ci=1; else ci=0; fi
if [ "$(tr -d '\r' < a.txt 2>/dev/null)" = lower ]; then result "case-only name collision restores the original" PASS "case-insensitive=$ci"
else result "case-only name collision restores the original" FAIL "case-insensitive=$ci a.txt=$(cat a.txt 2>&1) $(ls)"; fi

# 15 executable bit survives a rollback
fresh; printf '#!/bin/sh\necho hi\n' > x.sh; chmod +x x.sh
printf 'burn {\n  fs.write_text("x.sh", "changed")\n  cough "boom"\n}\n' > s.cig
run run s.cig
if [ "$WIN" = 1 ]; then result "executable bit survives rollback" SKIP "windows"
elif [ -x x.sh ] && grep -q 'echo hi' x.sh; then result "executable bit survives rollback" PASS
else result "executable bit survives rollback" FAIL "$(ls -l x.sh)"; fi

# 16 symlink survives a rollback
fresh; echo t > target.txt
if ln -s target.txt link.txt 2>/dev/null && [ -L link.txt ]; then
  printf 'burn {\n  fs.rm("link.txt")\n  cough "boom"\n}\n' > s.cig; run run s.cig
  [ -L link.txt ] && result "deleted symlink is restored as a symlink" PASS || result "deleted symlink is restored as a symlink" FAIL "$(ls -l) $(last)"
else result "deleted symlink is restored as a symlink" SKIP "cannot make symlinks here"; fi

# 17 a deep path (> 260 characters) rolls back
fresh; deep="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/cccccccccccccccccccccccccccccccccccccccccccccccccc/dddddddddddddddddddddddddddddddddddddddddddddddddd/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee/ffffffffffffffffffffffffffffffffffffffffffffffffff"
printf 'burn {\n  fs.mkdir("%s")\n  fs.write_text("%s/x.txt", "1")\n  cough "boom"\n}\n' "$deep" "$deep" > s.cig
run run s.cig
grep -q 'boom' out.txt && [ ! -e aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ] && result "deep path (>260 chars) rolls back" PASS || result "deep path (>260 chars) rolls back" FAIL "$(last)"

# 18 a killed run is reported interrupted and can be unburned
fresh; echo original > keep.txt
if [ "$WIN" = 1 ]; then sl='proc.run("powershell", ["-NoProfile", "-Command", "Start-Sleep 30"])'; else sl='proc.run("sleep", ["30"])'; fi
printf 'burn {\n  fs.write_text("keep.txt", "changed")\n  %s\n}\n' "$sl" > s.cig
"$CIG" run s.cig >/dev/null 2>&1 & P=$!; sleep 4
if [ "$WIN" = 1 ]; then taskkill //F //IM cig.exe >/dev/null 2>&1; else kill -9 "$P" 2>/dev/null; fi
wait "$P" 2>/dev/null; sleep 1
"$CIG" runs >runs.txt 2>&1; st=$(awk 'NR>1 {print $2; exit}' runs.txt)
id=$(ls "$CIGSCRIPT_HOME/runs" | tail -1); run unburn "$id"
[ "$st" = interrupted ] && [ "$(tr -d '\r' < keep.txt)" = original ] && result "killed run: interrupted, then unburned" PASS || result "killed run: interrupted, then unburned" FAIL "status=$st keep=$(cat keep.txt) runs: $(tr '\r\n' '  ' < runs.txt | cut -c1-200) | $(last)"

# 19 the state directory lands somewhere sensible without CIGSCRIPT_HOME
fresh; printf 'exhale 1\n' > s.cig
( unset CIGSCRIPT_HOME; "$CIG" run s.cig >/dev/null 2>&1; "$CIG" runs >runs.txt 2>&1; echo $? > code.txt )
[ "$(code)" = 0 ] && result "default state directory works" PASS || result "default state directory works" FAIL "$(tr '\r\n' '  ' < runs.txt | cut -c1-120)"

# 20 cig's own self-check runs (what ciginstaller doctor --tools reads; cig doctor hands over to it)
fresh; run __doctor
grep -q "state dir" out.txt && result "cig __doctor runs" PASS "exit $(code)" || result "cig __doctor runs" FAIL "$(last)"

# 21 the tin: a hop's edits and deletes come back (touches)
fresh; echo original > keep.txt; echo doomed > gone.txt
PY=$(command -v python3 || command -v python); PY=$(basename "$PY" .exe)
printf 'pack { "." }\nburn {\n  proc.run("%s", ["-c", "open(\x27keep.txt\x27,\x27w\x27).write(\x27HACKED\x27); import os; os.remove(\x27gone.txt\x27)"], {touches: ["**"]})\n  cough "boom"\n}\n' "$PY" > s.cig
run run s.cig
[ "$(tr -d '\r' < keep.txt)" = original ] && [ -f gone.txt ] && grep -q E600 out.txt && result "tin: a hop's edit and delete come back" PASS || result "tin: a hop's edit and delete come back" FAIL "$(last)"

# 22 the tin keeps an executable bit and a symlink
fresh; printf '#!/bin/sh\n' > x.sh; chmod 755 x.sh; echo t > t.txt; ln -s t.txt l.txt 2>/dev/null
printf 'pack { "." }\nburn {\n  proc.run("sh", ["-c", "chmod 644 x.sh; rm l.txt; echo plain > l.txt"], {touches: ["**"]})\n  cough "boom"\n}\n' > s.cig
if [ "$WIN" = 1 ]; then result "tin: executable bit and symlink come back" SKIP "windows"; else run run s.cig
  [ -x x.sh ] && [ -L l.txt ] && result "tin: executable bit and symlink come back" PASS || result "tin: executable bit and symlink come back" FAIL "x.sh=$(stat -c %a x.sh 2>/dev/null || stat -f %Lp x.sh) l.txt=$( [ -L l.txt ] && echo link || echo file)"; fi

# 23 the tin keeps modification times
fresh; echo v > old.txt; touch -t 202001020304 old.txt; before=$(ls -l old.txt | awk '{print $6,$7,$8}')
printf 'pack { "." }\nburn {\n  proc.run("sh", ["-c", "echo w > old.txt"], {touches: ["**"]})\n  cough "boom"\n}\n' > s.cig
if [ "$WIN" = 1 ]; then result "tin: modification time comes back" SKIP "windows"; else run run s.cig
  after=$(ls -l old.txt | awk '{print $6,$7,$8}'); [ "$before" = "$after" ] && result "tin: modification time comes back" PASS || result "tin: modification time comes back" FAIL "before=$before after=$after"; fi

# 24 the hash chain: a run verifies, an edited journal does not
fresh; printf 'burn {\n  fs.write_text("a.txt", "1")\n  fs.write_text("b.txt", "2")\n}\n' > s.cig
run run s.cig; id=$(ls "$CIGSCRIPT_HOME/runs" | tail -1); run runs "$id" --verify
if [ "$(code)" = 0 ] && grep -q "chain *intact" out.txt; then
  jp="$CIGSCRIPT_HOME/runs/$id/journal.jsonl"; "$PY" -c "import sys;p=sys.argv[1];l=open(p).read().split('\n');l[1]=l[1].replace('a.txt','z.txt',1);open(p,'w').write('\n'.join(l))" "$jp"
  run runs "$id" --verify
  [ "$(code)" != 0 ] && grep -q "BROKEN at line 3" out.txt && result "hash chain: intact run verifies, edited journal breaks" PASS || result "hash chain: intact run verifies, edited journal breaks" FAIL "$(last)"
else result "hash chain: intact run verifies, edited journal breaks" FAIL "$(last)"; fi

# 25 the tin in chunks: a one-byte edit to a large file comes back from its recipe
# 3 MiB sits above the chunk threshold (files 2 MiB and smaller are kept whole).
fresh; mkdir -p p; "$PY" -c "open('p/big.bin','wb').write(bytes((i*7+3)%251 for i in range(3<<20)))"
sum_before=$("$PY" -c "import hashlib;print(hashlib.sha256(open('p/big.bin','rb').read()).hexdigest())")
printf 'pack { "./p" }\nburn {\n  proc.run("%s", ["-c", "f=open(\x27big.bin\x27,\x27r+b\x27); f.seek(1500000); f.write(b\x27Z\x27)"], {cwd: "p", touches: ["**"]})\n  cough "boom"\n}\n' "$PY" > s.cig
run run s.cig
sum_after=$("$PY" -c "import hashlib;print(hashlib.sha256(open('p/big.bin','rb').read()).hexdigest())")
"$CIG" --json tin du > du.json 2>/dev/null
[ "$sum_before" = "$sum_after" ] && grep -q '"recipes":[1-9]' du.json && result "tin in chunks: large file comes back from its recipe" PASS || result "tin in chunks: large file comes back from its recipe" FAIL "same=$([ "$sum_before" = "$sum_after" ] && echo yes || echo no) $(cut -c1-120 du.json)"

# 26 sealed: a hidden file under touches comes back and is never in the clear
fresh; mkdir -p p; echo "SETTING=probe-hidden-value" > "p/.env"
printf 'pack { "./p" }\nburn {\n  proc.run("sh", ["-c", "echo SETTING=rotated > .env"], {cwd: "p", touches: ["**"]})\n  cough "boom"\n}\n' > s.cig
run run s.cig
if [ "$(tr -d '\r' < "p/.env")" = "SETTING=probe-hidden-value" ] && ! grep -rq "probe-hidden-value" "$CIGSCRIPT_HOME" 2>/dev/null && grep -q "(sealed)" out.txt; then result "sealed: a hidden file comes back and is never in the clear" PASS
else result "sealed: a hidden file comes back and is never in the clear" FAIL "file=$(cat "p/.env") clear=$(grep -rl probe-hidden-value "$CIGSCRIPT_HOME" 2>/dev/null | head -1) $(last)"; fi

# 27 git blob reuse: a clean tracked file is referenced, not copied
fresh; mkdir -p p
if git -C p init -q . 2>/dev/null && echo "tracked v1" > p/app.txt && git -C p add . && git -C p -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false commit -qm init; then
  printf 'pack { "./p" }\nburn {\n  proc.run("sh", ["-c", "echo v2 > app.txt"], {cwd: "p", touches: ["**"]})\n  cough "boom"\n}\n' > s.cig
  run run s.cig
  [ "$(tr -d '\r' < p/app.txt)" = "tracked v1" ] && grep -q "(git)" out.txt && git -C p for-each-ref refs/cig/ | grep -q "refs/cig/runs/" && result "git blob reuse: a clean tracked file is referenced" PASS || result "git blob reuse: a clean tracked file is referenced" FAIL "$(last)"
else result "git blob reuse: a clean tracked file is referenced" SKIP "no git"; fi

# 28 the burn protocol: a tool's compensate line runs on rollback
fresh; mkdir -p p
cat > p/tool.sh <<'TOOL'
#!/bin/sh
echo did > work.txt
printf '%s\n' '{"op":"compensate","label":"probe undo","undo":["sh","-c","echo undone > undone.txt"]}' >> "$CIG_JOURNAL"
TOOL
printf 'pack { "./p" }\nburn {\n  proc.run("sh", ["tool.sh"], {cwd: "p"})\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ -f p/undone.txt ] && grep -q "ran the compensation for probe undo" out.txt && result "burn protocol: a reported compensation runs on rollback" PASS || result "burn protocol: a reported compensation runs on rollback" FAIL "$(ls p) $(last)"

# 29 the parcel: a run unburns from another state directory
fresh; mkdir -p p; echo original > p/a.txt
printf 'pack { "./p" }\nburn {\n  proc.run("sh", ["-c", "echo changed > a.txt; echo new > b.txt"], {cwd: "p", touches: ["**"]})\n}\n' > s.cig
run run s.cig; id=$(ls "$CIGSCRIPT_HOME/runs" | tail -1); run parcel "$id" --out run.cigparcel
H2="$D-home2"; mkdir -p "$H2"
CIGSCRIPT_HOME="$H2" "$CIG" unparcel run.cigparcel --root "p=$D/p" > out.txt 2>&1 && CIGSCRIPT_HOME="$H2" "$CIG" unburn "$id" > out.txt 2>&1
[ "$(tr -d '\r' < p/a.txt)" = original ] && [ ! -e p/b.txt ] && result "parcel: imported elsewhere and unburned" PASS || result "parcel: imported elsewhere and unburned" FAIL "$(last)"

# 30 the box: a failing boxed hop changes nothing (Linux), or falls back and says so
fresh; mkdir -p p; echo original > p/keep.txt
printf 'pack { "./p" }\nburn {\n  proc.run("sh", ["-c", "echo changed > keep.txt; echo new > made.txt; exit 3"], {cwd: "p", box: true, ok: 0, touches: ["**"]})\n}\n' > s.cig
run run s.cig
if grep -q "box cannot be made here\|box could not be made here" out.txt; then
  [ "$(tr -d '\r' < p/keep.txt)" = original ] && result "the box: falls back to the tin and says so" PASS "no box here" || result "the box: falls back to the tin and says so" FAIL "$(last)"
elif grep -q "layer was dropped" out.txt && [ "$(tr -d '\r' < p/keep.txt)" = original ] && [ ! -e p/made.txt ]; then result "the box: a failing boxed hop changes nothing" PASS
else result "the box: a failing boxed hop changes nothing" FAIL "$(ls p) $(last)"; fi

# 31 a pipeline with touches is reversible like a hop
fresh; mkdir -p q; echo orig > q/a.txt
printf 'pack { "./q" }\nburn {\n  proc.pipe([["sh", "-c", "echo new > a.txt; echo made > made.txt; echo piped"], ["cat"]], {cwd: "q", touches: ["a.txt"]})\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ "$(tr -d '\r' < q/a.txt)" = orig ] && [ ! -e q/made.txt ] && result "pipeline: touches restored, created file removed" PASS || result "pipeline: touches restored, created file removed" FAIL "a.txt=$(cat q/a.txt) $(ls q) $(last)"

# 32 the burn protocol without a pack: the compensation still runs
fresh
cat > tool.sh <<'TOOL'
#!/bin/sh
printf '%s\n' '{"op":"compensate","label":"bare undo","undo":["sh","-c","echo undone > undone.txt"]}' >> "$CIG_JOURNAL"
TOOL
printf 'burn {\n  proc.run("sh", ["tool.sh"], {})\n  cough "boom"\n}\n' > s.cig
run run s.cig
[ -f undone.txt ] && grep -q "ran the compensation for bare undo" out.txt && result "burn protocol: a compensation counts without a pack" PASS || result "burn protocol: a compensation counts without a pack" FAIL "$(ls) $(last)"

echo "== $pass passed, $fail failed, $skip skipped"
