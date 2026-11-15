// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/sieve.cig: sieve of Eratosthenes, index writes into a big list.
// The list is built in a loop, as the cig program does; idiom/sieve.js makes it with
// Array.fill.
const n = 300000;
const flags = [];
for (let i = 0; i < n + 1; i++) {
  flags.push(true);
}
flags[0] = false;
flags[1] = false;
let p = 2;
while (p * p <= n) {
  if (flags[p]) {
    let m = p * p;
    while (m <= n) {
      flags[m] = false;
      m += p;
    }
  }
  p += 1;
}
let c = 0;
for (const b of flags) {
  if (b) {
    c += 1;
  }
}
console.log(c);
