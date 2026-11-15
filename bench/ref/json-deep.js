// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/json-deep.cig: a six-level nested document, parsed and walked.
const scale = parseInt(process.argv[2] ?? "1");
function mkdoc(d, n) {
  if (d === 0) {
    return { leaf: n };
  }
  const kids = [];
  for (let i = 0; i < n; i++) {
    kids.push(mkdoc(d - 1, n));
  }
  return { level: d, kids: kids };
}
function walk(x) {
  if ("leaf" in x) {
    return x.leaf;
  }
  let s = 0;
  for (const k of x.kids) {
    s += walk(k);
  }
  return s + x.level;
}
const doc = mkdoc(6, 3);
const body = JSON.stringify(doc);
let total = 0;
for (let i = 0; i < scale; i++) {
  const back = JSON.parse(body);
  total += walk(back);
}
console.log(total, body.length);
