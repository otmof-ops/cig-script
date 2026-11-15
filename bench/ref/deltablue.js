// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/deltablue.cig: DeltaBlue-like, a constraint chain.
const scale = parseInt(process.argv[2] ?? "1");
const n = 20 * scale;
const iters = 500;
const MOD = 1000003;
const vars = [];
for (let i = 0; i < n; i++) {
  vars.push(0);
}
let changes = 0;
for (let k = 0; k < iters; k++) {
  vars[0] = k;
  for (let i = 1; i < n; i++) {
    const prev = vars[i - 1];
    const want = (prev * 2 + i * 7 + 1) % MOD;
    if (vars[i] !== want) {
      vars[i] = want;
      changes += 1;
    }
  }
}
let total = 0;
for (const v of vars) {
  total = (total + v) % MOD;
}
console.log(total, changes);
