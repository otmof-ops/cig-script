// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/loop.cig: a tight numeric loop.
let total = 0;
for (let i = 0; i < 400000; i++) {
  total += (i * i) % 7;
}
console.log(total);
