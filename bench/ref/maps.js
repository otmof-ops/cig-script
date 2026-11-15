// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/maps.cig: maps, inserts and lookups.
const m = new Map();
for (let i = 0; i < 50000; i++) {
  m.set(`k${i}`, i);
}
let hits = 0;
for (let i = 0; i < 50000; i++) {
  if (m.has(`k${i * 2}`)) {
    hits += 1;
  }
}
console.log(hits);
