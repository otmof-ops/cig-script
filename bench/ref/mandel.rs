// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/mandel.cig: Mandelbrot, a tight float loop with an early exit.
// black_box: the size is not a constant the compiler can fold the work on.
use std::hint::black_box;

fn main() {
    let w: i64 = black_box(160);
    let h: i64 = black_box(160);
    let mut inside = 0;
    for y in 0..h {
        for x in 0..w {
            let cr = 2.0 * x as f64 / w as f64 - 1.5;
            let ci = 2.0 * y as f64 / h as f64 - 1.0;
            let mut zr = 0.0f64;
            let mut zi = 0.0f64;
            let mut k = 0;
            let mut escaped = false;
            while k < 50 && !escaped {
                let t = zr * zr - zi * zi + cr;
                zi = 2.0 * zr * zi + ci;
                zr = t;
                if zr * zr + zi * zi > 4.0 {
                    escaped = true;
                }
                k += 1;
            }
            if !escaped {
                inside += 1;
            }
        }
    }
    println!("{}", inside);
}
