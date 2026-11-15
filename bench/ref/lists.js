// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/lists.cig: lists, sort, filter and map.
const xs = [];
for (let i = 0; i < 60000; i++) {
  xs.push((i * 7919) % 10007);
}
// xs.sort() in cig gives a sorted copy and orders numbers as numbers.
const sorted = xs.slice().sort((a, b) => a - b);
const evens = sorted.filter((x) => x % 2 === 0);
const doubled = evens.map((x) => x * 2);
console.log(doubled.length, doubled[0], doubled[doubled.length - 1]);
