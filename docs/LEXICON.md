# The CigScript Lexicon

> Explain a thing by what it feels like, then attach the real name so nobody's
> embarrassed later.

Every entry has three parts: **your word**, **what it means**, and **the name the
manual uses**, so a reader who learns the vernacular can walk into a Lenovo forum
or a kernel bug tracker and recognise what people are talking about.

Entries marked **(coined 27 Sep 2026)** came out of one BIOS update and a
conversation about SSDs. Entries marked **(gap)** are processes that don't have a
word yet: tell me what you'd call them and they get promoted.

---

## The one rule

| your word | what it means | the manual's name |
|---|---|---|
| **nothing real happens unless you burn** | The only rule the language has. Everything else follows from it: a dry-run you can trust, rollback you never wrote, a run record that reads like a lab notebook. | effect fencing; capability discipline; "side effects are explicit" |
| **"nar, I like things done the easy way, actually"** | What CigScript says instead of screaming at you. The whole attitude in one line: the language has a professional case of the fuck-yous for languages that treat the user as the problem, and it refuses to be one. | the design ethic; "nobody's on trial"; hint-first, fix-first diagnostics |
| **the world** | Everything outside the script: files, processes, the network. The stuff a burn changes. | side effects; the environment; external state |
| **pure code** | Reads, maths, strings. Free; never touches the world. | pure computation; referentially transparent code |

---

## The language

| your word | what it means | the manual's name |
|---|---|---|
| **roll** | A variable you can change later. | mutable binding (`let`, `var`) |
| **stick** | A variable you can't change. Also: one step in a chain, because each stick is lit from the last. | constant binding (`const`); a task or step |
| **pull** | Define a named function. `pull tidy(dir) { … }` | function definition (`fn`, `def`) |
| **pack** | An unnamed function you can hand around. `pack(p) => …` or `pack(p) { … }`. Also, later: the environment a run lives in, and a bundle of chains. | lambda; closure; anonymous function |
| **snuff** | Leave the function and hand back a value. | `return` |
| **burn { }** | The only place the world can change. The kernel snapshots first, journals the op, and only then acts. | effectful block; transaction; unit of work |
| **burn unlit { }** | A burn that goes through every motion but never actually burns, even in a real run. Rehearsal. | simulated / no-op transaction; dry section |
| **cough** | Stop with an error. `cough "bad build"` | `throw`, `raise` |
| **try { } ashtray e { }** | Catch what got coughed. Where the mess lands. | `try` / `catch` |
| **exhale** | Print to the terminal. | `print`, `console.log` |
| **chain** | An ordered set of sticks that light from each other, fail together, and roll back together. `chain release { fetch, build, test, ship }` | task pipeline; job DAG; a Makefile target with dependencies |
| **light** | Run a chain. `cig light tasks.cig release`, or `light(release, {retries: 2})` from inside a script. | execute a task; invoke a pipeline |
| **chain smoking** | Running chains for a living. The task-runner half of the tool. | task runner; build orchestration |

---

## The CLI

| your word | what it means | the manual's name |
|---|---|---|
| **cig run** | Run a script for real. | execute |
| **cig run --dry-run** | The same run, but every burn is recorded instead of performed. It's the real run with the writes simulated, not a flag that pretends. | dry run; plan; `--noop`; `-WhatIf` |
| **the plan** | What a dry-run prints: every op that would burn, in order, each labelled reversible or irreversible. Folds never hide from it. | execution plan; change set |
| **cig check** | Typos, sticks, effects: refuse before anything runs. | static analysis; lint; type check |
| **cig unburn** | Put the world back the way it was, newest op first. | rollback; revert; restore |
| **cig runs** | The lab notebook: every run, its status, how many burns, how many irreversible. | audit log; run history |
| **cig doctor** | Hands the install check to the installer (`ciginstaller doctor --tools`); `cig doctor <run>` diagnoses one run's error and tells you what it found. Soon: fires by itself when an error code does, and explains why it thinks the error appeared and exactly what to do. | diagnostics; health check; self-repair |
| **cig explain E503** | The long version of an error, in plain words. | error documentation; `--explain` |
| **cig update** | Hands over to `ciginstaller update`, which gets the newer build, verified. | self-update |
| **Don't see any cigarettes.** | The first line of every error. Means: something you asked for isn't there, or isn't allowed here. | error banner |
| **burned 4 ops (1 irreversible)** | The receipt at the end of a run. | run summary |
| **ashtray** (the) | Where cig puts a report about its own bug (a crash, an internal error, a tripwire) before anything is sent, and where it stays once filed. `cig crash list` reads it; `cig crash flush` sends what is waiting; a run never waits on it. | local-first report queue; crash reporter spool |
| **tripwire** | A cheap check of one of cig's own promises, run after the work: a restored path re-hashed against the journal, the journal read back, a tin object, an applied box file. It never changes the run's outcome; it writes a report and says one line (`E902`). | post-condition check; self-check; invariant assertion |
| **repro** | The smallest script that still shows a bug, built from the run's journal with placeholder paths, generated bytes and stub hops, proved in a sandbox before it is attached, then shrunk. | reproducer; minimal reproduction; test case reduction |
| **cig report --send** | File a run's bundle as an issue through the ashtray, the way a crash is filed. | bug report submission |
| **reports = ask / always / never** | The one setting for every kind of report cig makes about itself. Ask once and remember; under CI with nothing set, never. | telemetry consent; opt-in crash reporting |

---

## The kernel (what happens under a burn)

| your word | what it means | the manual's name |
|---|---|---|
| **the kernel** | The one piece of code allowed to touch the world. Everything effectful funnels through it. | effect executor; transaction manager |
| **snapshot** | A copy of a file taken *before* the op that will change it. Plain copies, under your home directory, prune them if they hold anything sensitive. | backup; before-image; undo log entry |
| **the journal** | The list of ops a run performed, written *before* each op is performed, so a kill mid-burn still leaves a record you can unburn from. | write-ahead log (WAL); intent log |
| **newest first** | The order unburn replays the journal. The last thing done is the first thing undone. | reverse chronological rollback; LIFO undo |
| **reversible** | The kernel did it itself and has the snapshot to prove it. | undoable; compensable |
| **irreversible** | Something the kernel couldn't watch: a child process, the network. The plan says so out loud and never launders it. | non-compensable side effect |
| **the irreversible label** | The honesty guarantee. The one thing that must never be wrong, because the whole model is trusted through it. | provenance flag; effect classification |
| **run record** | Everything about one run, with an id like `20260925T203602-baa170`. | transaction log; run manifest |
| **interrupted run** | A run whose process died mid-burn. The journal is intact; the fix is `cig unburn`; doctor should point at it. | crashed transaction; dirty shutdown |
| **ghost filesystem** | During a dry-run, the pretend writes live in memory so later steps can read what earlier steps only pretended to write. | overlay; in-memory shadow FS; copy-on-write layer |

---

## Composition (the layers)

| your word | what it means | the manual's name |
|---|---|---|
| **stick → chain → pack → carton** | Sticks go in packs, packs go in cartons. Nobody has to be taught the hierarchy; they've seen it at a petrol station. | step → pipeline → module → package |
| **chain** | *Order.* Steps that light from each other, fail together, roll back together. | pipeline; DAG |
| **pack** (the composition sense) | *Scope and reuse.* Owns a declared environment (the paths it lives in), one journal for every chain inside, takes parameters. Roll back a pack and every chain in it comes back as one transaction. | module with a declared write set; transaction scope |
| **carton** | *Distribution.* A versioned set of packs with a manifest and a `cig >=` line: the thing you ship, install and update. | package; release artifact |
| **fold** | Promote tested automation upward: chains into a pack, packs into a carton, so you build on what already works instead of copy-pasting it. A fold is a checkpoint: anything that fails `cig check` can't be folded, with an error code to say why. | compose; encapsulate; link; bundle |
| **folded away** | Automation you built once and now light with one word. Not hidden: the plan still sees all the way down. | abstracted; encapsulated |
| **automation of automation** | Chains that light chains, packs that light packs, on a schedule or a watch. Unattended. | orchestration; scheduled pipelines; meta-workflows |
| **subtool** | A separate program with one job that only yarns with cig about the result. Big architecture moves become subtools so the kernel stays small enough to hold in your head. | plugin process; helper binary; sidecar |
| **yarn with cig** | What a subtool does: report back what it changed and whether it can be undone, in a fixed shape, and nothing else. | inter-process contract; effects manifest; protocol |
| **the burn protocol** | The one-page contract a foreign tool speaks to become a native step: append to `CIG_JOURNAL` what you changed, with the bytes before, and how to undo it. The kernel checks every word it can. `cig` is its own first speaker. `PROTOCOL.md`. | plugin protocol; effect reporting API |
| **the box** | A hop run over a copy-on-write layer on the pack (`{box: true}`: overlayfs in a user namespace on Linux), so a failing hop's changes never land at all instead of being put back; a hop that succeeds has its layer applied as native burns. Where the box cannot be made, the hop runs on the tin and says so. | sandboxed step; overlay |
| **the parcel** | One run's record, the tin objects and snapshots its journal needs and its compensations, as a single file (`cig parcel`), imported on another machine (`cig unparcel`, every file checked, the roots mapped) for `cig unburn` there. Not a carton: a carton is what you install, a parcel is what you undo. | run bundle; export |
| **sealed** | A tin object or snapshot encrypted at rest, with a key from the OS keyring (or a file only you can read where there is none): a secret-shaped name, a `.cigseal` entry or `{sensitive: true}` makes a file one. Named by a random id and never shared, so two equal secrets look nothing alike. | encrypted at rest; authenticated encryption (XChaCha20-Poly1305, STREAM) |
| **pressed** | A tin object written once and never rewritten: the name is the hash, a copy is given its name only once it hashes to it, and one that no longer does is replaced, never reused. | write-once; immutable object; content-addressed storage |
| **a recipe** | What a large file becomes in the tin: the ordered list of its chunks (each an object named by its own hash), with the hash and size of the whole. A one-byte change stores one chunk; the recipe is what the journal points at. | chunk index; content-defined chunking (FastCDC) manifest |
| **the hash chain** | Every journal line carries the hash of the line before it (format 2) or a signature under the tin's key (format 3), so a line edited, removed or reordered after the run no longer fits; `cig runs <id> --verify` walks it, with every other check `cig unburn` makes. Tamper evidence, not secrecy. | hash chain; tamper-evident log |
| **hop** | Crossing from CigScript into another program and back. Where guarantees die unless the hop is built to carry them. | process boundary; FFI call; subprocess invocation |
| **foreign script** | Someone's existing Python, bash, Node, Ruby or PowerShell, run inside a pack so it gains the plan and the rollback without being rewritten. Since 1.2 the pack watches what it creates, `touches` keeps what it may change in the tin, and cig-orchestrator runs whole projects' scripts this way; what is still to come is the script telling cig what it did, which is the burn protocol. | wrapped script; adapter-run task |
| **the doors** | The handful of APIs a language changes the world through (`open`, `shutil`, `fs`, `child_process`). Instrument the doors, leave the room alone. | effect surface; syscall wrappers; I/O boundary |

---

## Errors and diagnostics

| your word | what it means | the manual's name |
|---|---|---|
| **E1xx / E2xx / E3xx / E5xx / E6xx / E7xx** | Lex, syntax, check, runtime, cough, burn. Stable forever; a retired code is never reused. Reserved next: folds, hops, journal, ghost FS, ceilings. | error code families |
| **the hint** | The line under the error that tells you what to type next, not what went wrong. `put each statement on its own line, or separate them with ;` | fix-it suggestion; quick fix |
| **the spiderweb** | The planned shape of the error system: a code isn't a number, it's a node with ranked causes, a read-only probe for each, a remedy for each, and links to the codes that usually come before and after it. | diagnostic knowledge graph; error registry with causal links |
| **probe** | A read-only check doctor runs to confirm or rule out a cause. Never writes, never re-runs the failing command. | diagnostic check; health probe |
| **"I think… because I checked X and found Y"** | The only sentence doctor is allowed to say. Guesses are labelled guesses. | evidence-based diagnosis |
| **plain mode** | Same codes, same hints, no catchphrases, for logs, classrooms and the procurement person. | `--plain`; machine-friendly output |

---

## How you work (process words)

| your word | what it means | the manual's name |
|---|---|---|
| **hammer** | Hit one feature over and over on purpose until it gives something up. `HAMMER.md` is the list of where to swing. | adversarial testing; stress testing; abuse cases |
| **cheating** | Breaking the kernel by going *around* it instead of through it: symlinks pointing outside, kill signals mid-burn, editing files under it. The only way it's supposed to be breakable. | out-of-band failure; environmental fault injection |
| **the seam** | Where two things that each work meet and quietly don't. The chain dry-run reading a file the previous step only pretended to write. | integration boundary; interface bug |
| **boring bug** | One line that didn't know about symlinks. The kind of bug a correct design produces. Highest compliment a bug can get. | localised defect; non-architectural bug |
| **say no instead of breaking its values** | Refuse generously, permit carefully. A behaviour that's an error today can become a feature tomorrow without breaking anyone; a behaviour you allow by accident is one you can never take back. | fail-fast; forward-compatible strictness; "undefined is an error" |
| **too robust to break without cheating** | The goal for the kernel. | fault tolerance; defensive design |
| **peer reviewed** | What every update gets before engineers see it. Hand them the property test and the cheat list, and ask them to break it, not read it. | code review; QA sign-off |
| **the fingering stage** | See Hardware. Also, now, any phase where a system walks every part of itself to rebuild its map before it trusts it. | self-test; enumeration; warm-up |

---

## Hardware and firmware *(coined 27 Sep 2026)*

| your word | what it means | the manual's name |
|---|---|---|
| **the fingering stage** | First boot after a firmware update, black screen, keyboard lights blinking: the laptop is going down every memory lane and PCIe lane one at a time to make sure its mental map of the place is correct and shit is where it's supposed to be. 64 GB takes a while. Don't interrupt it. | memory training (memory reference code / MRC training); PCIe and USB enumeration; POST hardware init |
| **copping a feel** | The per-lane check inside the fingering stage: touch it, wait, confirm it's still where it was last time. | per-rank / per-lane timing calibration; link training |
| **didn't get a good fingering** | The firmware got interrupted before it finished its map, and now has stale NVRAM, a half-reset embedded controller, and ACPI tables from two different versions. Explains a lot of "random" issues. | corrupted / inconsistent firmware state; interrupted flash; stale NVRAM |
| **the flash** | Writing the new firmware. Interrupting it is the one thing that actually bricks a machine; waiting too long costs nothing. | BIOS/UEFI update; firmware write |
| **blinking is the good sign** | Lights or fans doing anything during a black screen means it's still working. A *repeating short pattern with pauses* is a diagnostic code and means it stopped. | activity indicator vs. LED error code |
| **the backup copy** | What a ThinkPad falls back to when a flash goes wrong, so the worst case is "rebooted twice and came back," not a paperweight. Same trick as the journal: write beside, not over; switch only when the new one proves itself. | dual BIOS; self-healing BIOS; A/B firmware |
| **deleted and overwritten** | Deleting frees the label, not the data. To destroy it you overwrite it, and on an SSD even that doesn't do it, because the controller writes to a fresh cell and leaves the old one until garbage collection feels like it. Only secure-erase or never-stored-unencrypted actually works. | file unlink vs. data sanitisation; TRIM; wear levelling; ATA Secure Erase; encryption at rest |
| **ThinkPads are resilient** | Statement of fact. | enterprise hardware durability; well-supported Linux platform |

### Why DDR5 is worse at it *(coined 27 Sep 2026, in anger)*

| your word | what it means | the manual's name |
|---|---|---|
| **I fucking hate DDR5** | Statement of fact, made in triplicate, from a laptop that was turned off. Memory was never this problematic until DDR5, and it isn't nostalgia. | DDR5 SDRAM (JEDEC JESD79-5); the generation where "it worked last week" stopped being evidence |
| **too sensitive** | The stick worked for months, the firmware got updated, and now it fails the check by a hair and the laptop loops with no screen. | tighter training tolerances; per-boot memory training with no fallback |
| **its own little power supply** | Every DDR5 stick carries a voltage regulator the firmware has to negotiate with. One more thing per stick that can answer late, answer wrong, or not answer at all, at boot and at shutdown. | the on-module PMIC (power management IC) |
| **the name tag** | The little ID the firmware reads off a stick before deciding whether to bother with it. Not "does it work," but "is it on my list." | the SPD (serial presence detect) profile; the firmware's memory compatibility table |
| **not seeing what it's expecting to see** | The stick is fine. The firmware read a name tag that isn't on its list and refused to go further. The fourth kind of no, done by a BIOS: eyes covered, RAM right there, "can't see any ciggies bro." | unsupported SPD profile after a firmware update; failed compatibility check; memory training loop |
| **the stick in the extra slot** | The one you added to the soldered one. When the two are different brands, speeds or ranks, it's the only unknown in the machine, so it's the one you pull first. | mixed-module configuration; soldered LPDDR/DDR plus SO-DIMM; rank/speed mismatch |
| **hasn't said goodnight** | At shutdown or sleep the firmware walks every stick's regulator through a power-down handshake. One stick answers a beat late, the firmware waits, the OS waits, and you get the standoff you've been blaming on Ubuntu. | memory power-down handshake failure; ACPI S5/S3 transition hang caused by a module |
| **a RAM bug in a trench coat** | A shutdown or sleep problem that was never the OS's fault. | firmware/memory-induced ACPI hang |
| **love big memory, hate DDR5** | The whole trade in one line: the capacity and bandwidth are why you bought it, the per-stick negotiation is why it hates you. | higher density and bandwidth vs. stricter training and power management |

Then the boring rule, which is the whole reason the section exists:

> Pull the stick you added. Boot on the soldered one. If it comes straight up,
> you've found it in one move. Live on it for a few days and count clean
> shutdowns; if the standoff goes with the stick, you've closed two bugs with
> one screwdriver. Then buy a stick that matches the soldered one's part
> number, and get your 64 back without the drama.

Flipping the laptop off first is optional but recommended. It is the
ceremony before the fix.

---

## The OS *(coined 27 Sep 2026)*

| your word | what it means | the manual's name |
|---|---|---|
| **Ubuntu doing Ubuntu things** | Any behaviour the OS produces that isn't a bug exactly, isn't a feature exactly, and would take an afternoon to explain. Reordering your boot entries. Treating "power off" as an opening bid. | distribution quirks; default-config surprises; udev/systemd/GRUB behaviour |
| **the shutdown standoff** *(proposed)* | Ubuntu asks the firmware to power off, the firmware consults its confused map, and both stand there waiting for the other to go first. Half the laptop's fault, half Ubuntu's. | ACPI shutdown hang; S5 transition failure |
| **the entry the firmware renamed** *(proposed)* | Where your OS went after a flash reset the boot order. Still there. `efibootmgr` sorts it out in one line. | EFI boot entry; NVRAM boot order reset |

---

---

## Scrounging a ciggy: how computers talk to each other

The way computers talk to each other is literally them trying to scrounge a
ciggy. They walk over, cover their eyes, and go *"I can't see any ciggies bro,
where are they at."* Then you have to point them right at it and retry the ask,
and they go *"oh yeah, here's my ciggies."*

| your word | what it means | the manual's name |
|---|---|---|
| **walking over** | Getting to the other machine before you can ask it anything. | opening a connection; the TCP handshake |
| **the ask** | What one computer says to another. | the request |
| **covering its eyes** | Asking without actually knowing where the thing is: wrong address, wrong name, wrong path, wrong port. The asker's fault, not the answerer's. | unresolved name; wrong route; bad URL; `ENOENT` |
| **"I can't see any ciggies bro"** | The reply when the ask points at nothing. The origin of the error banner: *Don't see any cigarettes.* | `404 Not Found`; `ECONNREFUSED`; `No such file or directory`; "not on PATH" |
| **pointing it right at it** | Fixing the address so the ask lands: the right hostname, the right port, the right path, the right directory on PATH. Doctor's whole job is telling you where to point. | DNS resolution; correct routing; the fix-it hint |
| **retry the ask** | Same ask, now aimed properly. | retry; retransmission; re-request |
| **"oh yeah, here's my ciggies"** | The other machine handing over what you asked for. | `200 OK`; the response body; the payload |
| **"apparently yous got ciggies for me over here"** | The ask when someone else told you who to walk up to. You're not guessing; you were sent. | a request following a referral: a DNS answer, a link, a redirect, a service-discovery result |
| **"aww yea"** | Got them, here you go. | `200 OK` |
| **"nup, never heard of you, fuck off m8"** | The ciggies are right there. It just doesn't know you, so you're not getting one. Different from "can't see any": that one's the asker's eyes, this one's the answerer's door. | `401 Unauthorized`; authentication failure; a firewall drop |
| **"these are MY ciggies, not yours"** | It knows exactly who you are and you're still not getting one. Also: someone else has the ciggies in hand right now and you'll have to wait. | `403 Forbidden`; permission denied; a file lock / `EBUSY` / "in use by another process" |
| **"I don't have any ciggies over here bro"** | You found the right bloke, he knows you, he'd hand them over if he had them. He hasn't got them. The fourth kind of no, and the only one that isn't anybody's fault. | `404` from the server's side; `410 Gone`; an empty result; `204 No Content` |
| **my ciggies** | The thing being handed over. | the response; the data; the packet |

### The four kinds of no

| what you hear | whose problem | the manual's name |
|---|---|---|
| "can't see any ciggies bro" | yours: you're pointed at the wrong place | wrong address, DNS, path, PATH |
| "never heard of you" | the door: it doesn't know you | 401 |
| "these are MY ciggies" | the door: it knows you and said no | 403, lock, permission denied |
| "don't have any over here" | nobody's: right place, right you, nothing there | 404 (server side), empty |

### The tone (why every error sounds like you're on trial)

The computer walking over to ask isn't *bad* about it. It's just very
accusatory about why it hasn't got its ciggies, and it makes every failure
sound like your fault, like you're on trial for it. And the other computer
saying no is a smug bastard about it: *"nar mate, nar, I've never seen any
ciggies over HERE before."*

| your word | what it means | the manual's name |
|---|---|---|
| **on trial** | The asker reporting a failure as if you personally did it, whoever's fault it actually was. Every stack trace ever. | conventional error messaging; "user error" framing; blame-first diagnostics |
| **the smug bastard** | The answerer that says no with total confidence and zero help, as if the thing you asked for had never existed anywhere. | a bare `404`; an error with no hint; "No such file or directory" and nothing else |
| **"nar mate, nar, I've never seen any ciggies over HERE before"** | The smug 404. Note the HERE: it's telling you it's the wrong place without telling you the right one. | `404 Not Found` with no redirect and no suggestion |

CigScript's diagnostics are deliberately the opposite of both. *Don't see any
cigarettes* is the asker admitting its own eyes are covered; the hint
underneath is someone pointing; and doctor's one permitted sentence, *I think
X because I checked Y and found Z*, is the un-smug version of no. Nobody is on
trial, and nobody gets to be a smug bastard.

### Gaps in this vocabulary (what do you call…)
- **(gap)** you asked, it walked off to get them, and never came back *(timeout)*
- **(gap)** it had the ciggies, went to hand them over, and dropped them all on the floor *(500 Internal Server Error; a crash mid-response)*
- **(gap)** asking around "who's got ciggies?" before you know who to walk up to *(DNS lookup; service discovery)*
- **(gap)** too many people asking one bloke for ciggies at once, and him telling everyone to slow down *(429 Too Many Requests; rate limiting; backpressure)*
- **(gap)** the bloke who says "not me, but him over there" *(301/302 redirect; a proxy)*
- **(gap)** handing over half a ciggy and saying that's all there is *(partial content; truncated response)*
- **(gap)** the handshake where you both check you're who you say you are before any ciggies change hands *(TLS handshake)*

---

## Fibre: the ciggy has to hit right

| your word | what it means | the manual's name |
|---|---|---|
| **splitting and re-rolling** | You haven't got enough for two ciggies, so you split the tobacco and roll it into two thinner ones. One fibre from the exchange, shared out to a whole street. Everyone gets a ciggy; nobody gets a full one. | a PON optical splitter (1:32, 1:64); shared / contended bandwidth |
| **not hitting the way it needs to** | The ciggy's lit, it's in your mouth, it's just not giving you the fix. The link is up; it's the throughput that's thin. Half the tobacco, half the hit. | reduced bandwidth; contention during peak; under-provisioned link; low optical power at the receiver |
| **the fix** | What you actually wanted from the connection: the video not buffering, the call not stuttering, the download finishing. The thing bandwidth exists to deliver. | throughput as experienced; quality of service; goodput |
| **a full ciggy** | One whole roll, unshared, hitting exactly as it should. | a dedicated / uncontended link; point-to-point fibre |

### Gaps in this vocabulary (what do you call…)
- **(gap)** joining two fibres end-to-end so the light doesn't notice *(a fusion splice)*
- **(gap)** a splice done badly, where a bit of the light falls out at the join *(splice loss; insertion loss, dB)*
- **(gap)** bending the fibre so tight the light leaks out the side *(a macrobend)*
- **(gap)** the trace that shows you exactly how far along the run someone put a shovel through it *(an OTDR trace; the reflection at a break)*
- **(gap)** the light being too weak by the time it gets to your house *(attenuation; low receive power, dBm)*
- **(gap)** the light being too strong and blinding the receiver *(receiver overload / saturation)*
- **(gap)** the box on the wall the fibre ends in, called something different on every network *(the ONT / NTD / ONU)*
- **(gap)** a dirty connector face that wrecks the whole link *(end-face contamination; "inspect before you connect")*
- **(gap)** everyone on the street lighting up at 7 pm at once *(peak contention; the busy hour)*
- **(gap)** the exchange end, where the single fibre starts *(the OLT)*
- **(gap)** upload being a thinner ciggy than download *(asymmetric provisioning)*

---

## Rules for coining a new one

1. **Name it by what it feels like.** The vendor's word describes the mechanism; yours describes the experience. Both are needed, in that order.
2. **It has to map cleanly.** A reader who learns "the fingering stage" should meet "memory training" in the wild and go "oh, that." If it doesn't map, it's a meme, not a term.
3. **It has to work in a hallway.** If you'd say it out loud to explain the problem to a colleague, it's a keyword. If you'd only write it, it's documentation.
4. **The joke is the mnemonic.** It's allowed to be funny *because* funny things stick. It's not allowed to be funny *instead of* accurate.
5. **Attach the real name every time.** Nobody gets embarrassed later, and the term earns its way into serious rooms.

---

## Gaps: what do you call…

Processes a computer goes through that don't have a word yet. Answer any of
them and they get promoted to the tables above.

### Boot
- **(gap)** the very first self-check before anything loads, beeps and all *(POST)*
- **(gap)** the tiny program whose only job is to find and start the real OS *(bootloader, GRUB)*
- **(gap)** the moment the firmware hands over to the OS and stops being in charge *(kernel handoff / ExitBootServices)*
- **(gap)** the temporary mini-OS the kernel uses to find its own disk *(initramfs)*
- **(gap)** the firmware's little scratchpad of settings that a reset wipes *(NVRAM / CMOS)*
- **(gap)** the chip that stays awake to manage the keyboard, battery and fans even when the CPU is off *(embedded controller, EC)*

### Sleep and power
- **(gap)** light sleep: screen off, RAM kept alive *(S3 suspend)*
- **(gap)** the newer "sleep" where the CPU keeps twitching to check notifications and drains your battery in a bag *(S0ix / modern standby)*
- **(gap)** deep sleep to disk *(hibernate, S4)*
- **(gap)** the fans and clock backing off because it got too hot *(thermal throttling)*
- **(gap)** the CPU briefly running faster than rated because it can *(turbo boost)*

### The CPU
- **(gap)** the four-beat rhythm every instruction goes through *(fetch, decode, execute, write-back)*
- **(gap)** the CPU guessing which way an `if` will go and starting work early *(branch prediction)*
- **(gap)** the guess being wrong and all that early work getting thrown away *(branch misprediction / pipeline flush)*
- **(gap)** the tiny fast memories next to the core, and the thing you want being in one *(cache; cache hit)*
- **(gap)** the thing you want not being in any of them, and the long walk to RAM *(cache miss)*
- **(gap)** the OS yanking one program off the core to give another a turn *(context switch)*
- **(gap)** hardware tapping the CPU on the shoulder mid-instruction *(interrupt)*
- **(gap)** one core pretending to be two *(hyper-threading / SMT)*

### Memory
- **(gap)** every program being lied to that it has the whole machine to itself *(virtual memory)*
- **(gap)** the lie being caught and the OS quietly fetching the real page *(page fault)*
- **(gap)** RAM spilling onto disk because it's full *(swap)*
- **(gap)** a program that keeps asking for memory and never gives it back *(memory leak)*
- **(gap)** the runtime coming through to collect what nobody's using anymore *(garbage collection)*
- **(gap)** the OS killing the biggest program to save the rest *(OOM killer)*
- **(gap)** writing past the end of the space you were given *(buffer overflow)*

### Storage
- **(gap)** the filesystem's own version of your journal *(journaling filesystem)*
- **(gap)** telling the disk "actually write it now, I mean it" *(fsync)*
- **(gap)** the SSD being told which blocks are free so it can clean up in the background *(TRIM)*
- **(gap)** the SSD spreading writes around so no cell wears out first *(wear levelling)*
- **(gap)** a file's real identity, separate from its name *(inode)*
- **(gap)** a name that points at another file's name *(symlink; the one that came back as a regular file)*
- **(gap)** one file with two names *(hard link)*
- **(gap)** the drive lying about how much space you have because of the copy-on-write thing *(CoW snapshots / thin provisioning)*

### Processes and the OS
- **(gap)** the OS deciding whose turn it is on the CPU *(the scheduler)*
- **(gap)** a program that finished but whose parent hasn't collected the body *(zombie process)*
- **(gap)** a child still running after its parent died *(orphan process)*
- **(gap)** a program that runs in the background forever with no face *(daemon / service)*
- **(gap)** the OS's polite "please stop" versus the one it can't refuse *(SIGTERM vs SIGKILL)*
- **(gap)** cloning a running program then swapping its brain for a different one *(fork / exec)*
- **(gap)** two programs each waiting for the other to let go *(deadlock)*
- **(gap)** two things racing for the same resource and the answer depending on who's faster *(race condition)*
- **(gap)** the kernel itself falling over and the machine freezing *(kernel panic)*
- **(gap)** the pile of memory a crashed program leaves behind for you to look at *(core dump)*
- **(gap)** the list of places the OS looks for a command *(PATH; where `kubectl` wasn't)*

### Networking
- **(gap)** asking the router for an address and being lent one for a while *(DHCP lease)*
- **(gap)** turning a name into a number *(DNS lookup)*
- **(gap)** the three-step "hello / hello back / okay" before any data moves *(TCP handshake)*
- **(gap)** a packet not showing up and being sent again *(retransmission / packet loss)*
- **(gap)** the whole house sharing one public address *(NAT)*
- **(gap)** the bouncer deciding which packets get in *(firewall)*
- **(gap)** the secret-handshake version of the handshake *(TLS)*
- **(gap)** how long a round trip takes vs how much fits down the pipe *(latency vs bandwidth)*
- **(gap)** the laptop hopping between access points and dropping the call *(Wi-Fi roaming)*

### Building software
- **(gap)** turning source into a runnable thing *(compile)*
- **(gap)** stitching the pieces together into one program *(link)*
- **(gap)** the package manager working out which versions can all live together *(dependency resolution)*
- **(gap)** the file that freezes those versions so it's the same on every machine *(lock file)*
- **(gap)** a test that passes sometimes *(flaky test)*
- **(gap)** something that used to work and now doesn't *(regression)*
- **(gap)** the one line that runs a million times and decides whether it's fast *(hot path)*
- **(gap)** counting from zero and getting it wrong by one *(off-by-one)*
- **(gap)** doing six unrelated things to get to the one thing you meant to do *(yak shaving)*
- **(gap)** the whole team arguing about the colour of the button *(bikeshedding)*
- **(gap)** the pile of shortcuts you'll pay for later *(tech debt)*

### Security
- **(gap)** a one-way fingerprint of data *(hash)*
- **(gap)** scrambling data so only the key holder can read it *(encryption)*
- **(gap)** the firmware refusing to boot anything that isn't signed *(Secure Boot)*
- **(gap)** the tamper-proof chip that holds the keys *(TPM)*
- **(gap)** running something in a box so it can't reach the rest of the machine *(sandbox; what your kernel explicitly is not)*
- **(gap)** going from normal user to root through a crack *(privilege escalation)*
- **(gap)** a hole nobody knew about until someone used it *(zero-day)*