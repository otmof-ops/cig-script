// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// The maps of bench/compute/maps.cig with a plain object for the map, the older
// JavaScript habit, where ../maps.js uses a Map. Object.create(null) has no prototype,
// so no key can collide with an inherited name.
const m = Object.create(null);
for (let i = 0; i < 50000; i++) {
  m[`k${i}`] = i;
}
let hits = 0;
for (let i = 0; i < 50000; i++) {
  if (`k${i * 2}` in m) {
    hits += 1;
  }
}
console.log(hits);
