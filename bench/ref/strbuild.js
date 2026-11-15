// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/strbuild.cig: string building, s += piece.
const scale = parseInt(process.argv[2] ?? "1");
const n = 10000 * scale;
let s = "";
for (let i = 0; i < n; i++) {
  s += "x";
}
console.log(s.length);
