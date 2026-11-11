<!--
SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
SPDX-License-Identifier: FSL-1.1-ALv2
-->

# cig-pass

One primitive for every "this was already checked" in the cig family.

A **pass** is a small authenticated record: what was checked, every input
the check depended on (recorded while it ran), the result, and the caveats
under which it holds. Checking it again costs a MAC or a signature and a
look at each input, whatever the work it stands for.

- **Local** passes carry an HMAC-SHA256 chain; anyone holding one can add a
  caveat and nobody can remove one. **Shareable** passes carry an Ed25519
  chain another machine verifies against allowed signers scoped per kind.
- `Recorder` collects inputs while the work runs; `issue_local` and
  `issue_shareable` make the pass; `attenuate` narrows it; `verify` (the
  only code that decides "trusted") checks it against a `Ctx` the caller
  implements; `store` keeps passes, `marks` caches the state the caller
  folds from its ledger.
- The crate does no key management: the caller derives keys and answers the
  verifier's questions through `Ctx` (`MemCtx` answers them from memory).

cig's guide is `docs/PASS.md` in this repository; the threat model is
[THREAT-MODEL.md](THREAT-MODEL.md). Licensed under FSL-1.1-ALv2.
