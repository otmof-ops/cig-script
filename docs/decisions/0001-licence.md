<!-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script) -->
<!-- SPDX-License-Identifier: CC-BY-4.0 -->

# 0001: License of CigScript

- Status: accepted
- Date: 2026-11-11
- Issue: otmof-ops/cig-installer#27

## Context

CigScript was public under Apache-2.0. Issue #27 sets out the aim for the family:
free to use, study and change, with one thing prevented, which is reselling it.
It plans the change for carton 1.0.0 (#25). Everything released before stays
Apache-2.0, as it was released, and cannot be taken back. ciginstaller made the
same move on 2026-11-10 (its decision 0002).

## Decision

From the licensing commit on, the code is under **FSL-1.1-ALv2**, the
Functional Source License 1.1 with an Apache-2.0 future license. Each version
converts to Apache-2.0 on the second anniversary of the date it is made
available. Documentation stays CC-BY-4.0. The copyright line is
`Copyright 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)`.

Contributions come in under the contributor agreement (ICLA and CCLA, the same
as CigScript's and ciginstaller's with the project name changed); the DCO is
dropped. [LICENSING.md](../../LICENSING.md) holds the table of which versions
carry which license.

## Options considered

| License | Why not |
|---|---|
| Apache-2.0 (unchanged) | Allows resale, which is the thing #27 wants to prevent. |
| BUSL-1.1 | Needs an Additional Use Grant written by the licensor, and that grant is where "resale" would have to be defined. Wording that has to be right and cannot be borrowed. |
| Elastic-2.0 | Bars hosted managed services but does not stop selling copies. |
| PolyForm Noncommercial 1.0.0 | Bars every commercial use, so a company could not use it internally. |
| PolyForm Small Business 1.0.0 | Bars organizations above its size limit from using it at all. |
| FSL-1.1-MIT | Same terms with MIT as the future license; Apache-2.0 matches the rest of the family and carries an explicit patent grant. |
| **FSL-1.1-ALv2** | Chosen. The text is fixed and unmodified, so there is no grant to write. It allows internal use, study, changes and sharing, bars selling it or offering it in a competing product or service, and converts each version to Apache-2.0 after two years. |

## Consequences

- **Not open source.** The OSI does not recognize FSL. The README and
  LICENSING.md say source-available, and some organizations and distributions
  will not ship or depend on it.
- **The Apache era.** Every release through v1.20.0 was made under Apache-2.0 and stays under it. The last Apache-2.0 commit is marked by the
  annotated tag `apache-2.0-last` and recorded in LICENSING.md. That grant is
  irrevocable for any copy taken.
- **Contributions.** A CLA lets the project relicense and convert contributions
  that a DCO would not. Signing is a one-time comment on a pull request,
  recorded by a bot. `git shortlog -se` on 2026-11-11 showed no outside human
  contributor, so nothing needs re-collecting.
- **The text.** LICENSE is the publisher's text (fsl.software, identical to the
  SPDX License List copy), with only its notice line filled in: the year and
  the licensor. Legal review of the text: not applicable, since it is
  unmodified.
- **Trademarks.** FSL grants no trademark rights. Forks are possible under its
  terms but need their own name; TRADEMARKS.md says so.
- **The embedded runtime.** A program built with CigCompile carries cig. From the first release under FSL-1.1-ALv2 the cig inside is FSL-1.1-ALv2 code; programs built with earlier cig releases keep Apache-2.0. CigCompile's documentation says what that means for the person who passes a program on.
