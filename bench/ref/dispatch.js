// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/dispatch.cig: functions kept in a map, called by key.
const ops = {
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => (a * b) % 1000003,
};
const names = ["add", "sub", "mul"];
let acc = 1;
for (let i = 0; i < 150000; i++) {
  const f = ops[names[i % 3]];
  acc = ((f(acc, i) % 1000003) + 1000003) % 1000003;
}
console.log(acc);
