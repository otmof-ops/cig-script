// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/trees.cig: binary trees, allocation and recursion.
function make(d) {
  if (d === 0) {
    return [null, null];
  }
  return [make(d - 1), make(d - 1)];
}
function check(t) {
  if (t[0] === null) {
    return 1;
  }
  return 1 + check(t[0]) + check(t[1]);
}
const maxd = 12;
console.log("stretch", check(make(maxd + 1)));
const long = make(maxd);
let d = 4;
while (d <= maxd) {
  const n = Math.pow(2, maxd - d + 4);
  let c = 0;
  for (let i = 0; i < n; i++) {
    c += check(make(d));
  }
  console.log(n, d, c);
  d += 2;
}
console.log("long", check(long));
