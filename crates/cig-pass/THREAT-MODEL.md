<!--
SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
SPDX-License-Identifier: Apache-2.0
-->

# cig-pass: threat model

`cig_pass::verify` is the one place in the cig family that decides
"trusted". This page says what a pass claims, who it holds against, how,
and what it cannot do. Re-read it, and section 6, whenever the code changes.

## 1. What a pass claims

A pass made by `issue_local` or `issue_shareable` says: a piece of work of
kind *K* about subject *S* returned result *R*; while it ran it read exactly
the recorded inputs; it holds under the caveats it carries. `verify` says
yes only when the tag or signature chain is intact, the issuer is trusted
for *K*, its epoch is current, nothing it was narrowed from is revoked,
every caveat holds for the action the caller states, and every input is as
recorded now.

It does not say the result is *true*: only that the work, redone on the same
inputs, would see what it saw.

## 2. Assets and keys

- The tin's master key (keyring, `seal.key`, or a descriptor). Every pass
  key is an HKDF subkey of it: `cigpass1/local\0<kind>\0<epoch>` per kind and
  epoch, `pass-issuer`, `pass-hash` (keyed digests in local passes),
  `pass-sign` (the Ed25519 seed for shareable passes), `pass-marks` (the
  state cache's MAC). No label is reachable from `cig tin mac`, whose labels
  are `app:`-prefixed.
- The ledger: spends, revocations and epoch moves are MAC-chained lines.
- The keys folder: the passes, the state cache, the allowed signers.

## 3. Trust boundaries and adversaries

| adversary | can | cannot |
|---|---|---|
| another user on the machine | read what is world-readable | read or write the keys folder (0700, checked on every open) |
| a boxed hop | run as you inside the box | see the keys folder (an empty tmpfs covers it), so neither read the key nor forge, roll back or delete state |
| a holder of a token | narrow it, present it | widen it, drop a caveat, re-spend a `once`, make it hold after an input changed |
| a remote holder of a shareable pass | present it to a machine that trusts its signer for its kind | have it trusted for `approval`, `verdict`, `rules`, `doctor`, `trial` or `run` (never foreign), or as a `human-yes` |
| a process of yours, outside a box, that can write the keys folder but not read the key | delete or put back files | forge a pass; delete the ledger without every pass made after its first line refusing to hold |
| a process of yours, outside a box, that can read the key | everything a local pass protects | (nothing: see section 5) |

## 4. Attacks and what stops them

| attack | stopped by |
|---|---|
| forge or alter a pass | HMAC-SHA256 chain (local), Ed25519 `verify_strict` chain (shareable); constant-time compares; strict decoding (one encoding per pass; unknown version, kind, input, caveat, flag refused) |
| drop or reorder a caveat | each caveat is chained into the next tag or signed into the next block |
| narrow a sealed shareable pass | the proof is a final signature, not a key |
| spend a `once` twice | the spend key is the id through that caveat, so later narrowings share it; counts live in the ledger; a spend counts under the ledger's exclusive lock after checking the counts again |
| two writers fork the ledger | one exclusive lock around every append |
| resurrect a revoked pass by deleting the cache | the cache is folded from the ledger; a missing cache is rebuilt, never reset |
| delete the ledger and the cache together | every local pass carries the ledger's length when it was made; a shorter ledger refuses it |
| put back an older cache | the cache is pinned to the ledger's length and last line; a mismatch refuses every pass (E739) |
| stale "yes" after an input changed | every input re-identified: stat identity for a local pass's calm files, the content hash otherwise (racy files, shareable passes, network and FUSE file systems, pseudo file systems such as `/proc` and `/sys` whose times do not move when their contents do, Windows) |
| a same-tick edit with the times put back | the racy flag (mtime or ctime within two seconds of the read), hashed at every verify |
| a value the work saw but did not record | the recorder sits in every read builtin; an unclassified builtin is open-world; outer variables are refused; a global is recorded by a digest of its value; a hop needs `reads` and gets a fixed, recorded environment |
| a pass made from a dry run's simulated world | a dry run issues nothing |
| a hostile token exhausting the verifier | at most 1,024 nested passes, 1,000,000 input checks and 1 GiB hashed per verify; inputs opened without blocking and hashed only as regular files |
| hash flooding the store | each segment's index is keyed with its own random salt; probes are capped |
| a planted signer | the allowed-signers file is read only when it is ours, a regular file and closed to others' writes; each key is trusted only for the kinds its namespaces name |
| a forged `human-yes` | only an authority key from a root-owned file is accepted, and a `human-yes` must expire within the hour |

## 5. What it cannot protect against

- A process of yours, outside a box, that can read the tin's key can make
  any local pass. The keyring and the key file are both reachable by such a
  process. An approval records that cigo asked and you answered; against
  such a process it is not a wall. The human authority is the answer for a
  yes that must not be mintable by your own processes, and none is
  installed by default.
- A whole keys folder put back from an older copy (ledger and cache
  together, consistently) cannot be told apart locally. `cig tin head` is
  the anchor to keep elsewhere.
- Spends are per machine.
- A hop's `reads` is a declaration. Its libraries, interpreter and network
  are not seen (the box's read watch is future work).
- `agent` is a label the caller states, not authentication. Every action
  caveat is checked against what the caller says it is about to do; the
  caller must build that from what it really runs, and the box, not the
  pass, holds the program to it.
- Between `verify` and use, a file can change. A caller that runs a file
  should run the bytes it checked.

## 6. What a reviewer must re-check when the code changes

1. A new builtin (cig): classify it in `src/interp/record.rs`; the default
   is open-world.
2. A new input, caveat or flag: strict decoding, ordering, the kind policy,
   the text sanitized before printing, a test that the pass stops holding
   when the element changes.
3. The order inside `verify`: kind policy, then tag or signatures, issuer,
   epoch, ledger length, revocation, caveats, inputs. Nothing before the tag
   touches a path the pass names.
4. The ledger or spend code: the lock, counts checked again under it,
   `pass-*` lines never shaped like a run's.
5. The state cache: folded from the ledger, pinned to it, named by its key.
6. Key derivation: labels distinct and versioned; the epoch in the info.
7. The identity shortcut: local passes only, the racy flag, the file-system
   check (network, FUSE and pseudo file systems always hashed).
8. Any compare of a tag, key or digest that decides: constant time.
9. Any parse of outside bytes (tokens, store records, segments, allowed
   signers): lengths checked before allocation, never a panic, the fuzz
   targets updated.
10. Any file in `passes/`: `create_new`, 0600, no link followed, a random
    temporary name, rename in place.
11. A new caller of `verify` or `spend`: it states the action it will really
    run, and uses the same statement for both.
12. A new kind: its row in `src/policy.rs`.
13. `export` and `import`: sealed by default, an expiry added, `once`, `max`,
    `needs` and variable inputs refused, never-foreign kinds refused.
14. The box: the keys folder stays covered.
15. Dependencies: `hmac`, `sha2`, `blake3`, `ed25519-dalek` (with `zeroize`,
    `verify_strict`), `subtle`, `base64`, `globset`. A new crypto crate is a
    change to this page.
