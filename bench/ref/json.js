// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/json.cig: JSON out and back in.
const rows = [];
for (let i = 0; i < 20000; i++) {
  rows.push({ id: i, name: `n${i}`, tags: ["a", "b"], ok: i % 3 === 0 });
}
const body = JSON.stringify(rows);
const back = JSON.parse(body);
console.log(back.length, body.length);
