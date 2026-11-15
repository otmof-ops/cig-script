// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// The sieve of bench/compute/sieve.cig as JavaScript is usually written: the list
// made at its full length by Array.fill, and for loops where the cig program has
// while loops. ../sieve.js is the statement-for-statement version.
const n = 300000;
const flags = new Array(n + 1).fill(true);
flags[0] = false;
flags[1] = false;
for (let p = 2; p * p <= n; p++) {
  if (flags[p]) {
    for (let m = p * p; m <= n; m += p) {
      flags[m] = false;
    }
  }
}
let c = 0;
for (const b of flags) {
  if (b) {
    c += 1;
  }
}
console.log(c);
