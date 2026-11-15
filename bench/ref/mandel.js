// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/mandel.cig: Mandelbrot, a tight float loop with an early exit.
const w = 160;
const h = 160;
let inside = 0;
for (let y = 0; y < h; y++) {
  for (let x = 0; x < w; x++) {
    const cr = (2.0 * x) / w - 1.5;
    const ci = (2.0 * y) / h - 1.0;
    let zr = 0.0;
    let zi = 0.0;
    let k = 0;
    let escaped = false;
    while (k < 50 && !escaped) {
      const t = zr * zr - zi * zi + cr;
      zi = 2.0 * zr * zi + ci;
      zr = t;
      if (zr * zr + zi * zi > 4.0) {
        escaped = true;
      }
      k += 1;
    }
    if (!escaped) {
      inside += 1;
    }
  }
}
console.log(inside);
