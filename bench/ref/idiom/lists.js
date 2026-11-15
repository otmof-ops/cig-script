// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// The lists of bench/compute/lists.cig with a typed array's sort: the numbers (all under
// 2^31, so an Int32Array holds them exactly) are copied into an Int32Array, whose
// sort() orders numbers natively with no comparator, then back to an array.
// ../lists.js is the statement-for-statement version (Array.prototype.sort with a
// comparator).
const xs = [];
for (let i = 0; i < 60000; i++) {
  xs.push((i * 7919) % 10007);
}
const sorted = Array.from(Int32Array.from(xs).sort());
const evens = sorted.filter((x) => x % 2 === 0);
const doubled = evens.map((x) => x * 2);
console.log(doubled.length, doubled[0], doubled[doubled.length - 1]);
