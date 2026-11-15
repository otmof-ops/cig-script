// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/fannkuch.cig: fannkuch-redux, list indexing and swaps.
const n = 8;
const perm1 = [];
for (let i = 0; i < n; i++) {
  perm1.push(i);
}
const count = [];
for (let i = 0; i < n; i++) {
  count.push(0);
}
let maxflips = 0;
let checksum = 0;
let permcount = 0;
let r = n;
let done = false;
while (!done) {
  while (r !== 1) {
    count[r - 1] = r;
    r -= 1;
  }
  const perm = perm1.slice(0);
  let flips = 0;
  let k = perm[0];
  while (k !== 0) {
    let i = 0;
    let j = k;
    while (i < j) {
      const t = perm[i];
      perm[i] = perm[j];
      perm[j] = t;
      i += 1;
      j -= 1;
    }
    flips += 1;
    k = perm[0];
  }
  if (flips > maxflips) {
    maxflips = flips;
  }
  if (permcount % 2 === 0) {
    checksum += flips;
  } else {
    checksum -= flips;
  }
  let more = true;
  while (more) {
    if (r === n) {
      done = true;
      more = false;
    } else {
      const p0 = perm1[0];
      let i = 0;
      while (i < r) {
        perm1[i] = perm1[i + 1];
        i += 1;
      }
      perm1[r] = p0;
      count[r] = count[r] - 1;
      if (count[r] > 0) {
        more = false;
      } else {
        r += 1;
      }
    }
  }
  permcount += 1;
}
console.log(checksum, maxflips);
