// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/closures.cig: closures made in a loop and called.
function counter(start) {
  let n = start;
  return () => n + 1;
}
let total = 0;
for (let i = 0; i < 100000; i++) {
  const f = counter(i);
  total += f();
}
console.log(total);
