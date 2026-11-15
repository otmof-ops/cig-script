// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/spectral.cig: spectral norm, float loops and calls.
function a(i, j) {
  return 1.0 / (((i + j) * (i + j + 1)) / 2 + i + 1);
}
function mulav(v, n) {
  const out = [];
  for (let i = 0; i < n; i++) {
    let s = 0.0;
    for (let j = 0; j < n; j++) {
      s += a(i, j) * v[j];
    }
    out.push(s);
  }
  return out;
}
function mulatv(v, n) {
  const out = [];
  for (let i = 0; i < n; i++) {
    let s = 0.0;
    for (let j = 0; j < n; j++) {
      s += a(j, i) * v[j];
    }
    out.push(s);
  }
  return out;
}
const n = 100;
let u = [];
for (let i = 0; i < n; i++) {
  u.push(1.0);
}
let v = [];
for (let k = 0; k < 10; k++) {
  v = mulatv(mulav(u, n), n);
  u = mulatv(mulav(v, n), n);
}
let vbv = 0.0;
let vv = 0.0;
for (let i = 0; i < n; i++) {
  vbv += u[i] * v[i];
  vv += v[i] * v[i];
}
// math.round(x, 9): JavaScript has no rounding to a number of places.
console.log(Math.round(Math.sqrt(vbv / vv) * 1e9) / 1e9);
