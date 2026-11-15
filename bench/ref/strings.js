// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/strings.cig: building a long string from pieces.
const parts = [];
for (let i = 0; i < 60000; i++) {
  parts.push(`item-${i}`);
}
const s = parts.join(",");
console.log(s.length);
