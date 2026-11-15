// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/loop.cig: a tight numeric loop.
// black_box: the size is not a constant the compiler can fold the work on.
use std::hint::black_box;

fn main() {
    let n: i64 = black_box(400000);
    let mut total: i64 = 0;
    for i in 0..n {
        total += i * i % 7;
    }
    println!("{}", total);
}
