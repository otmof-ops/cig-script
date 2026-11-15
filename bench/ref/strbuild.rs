// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/strbuild.cig: string building, s += piece.
fn main() {
    let scale: i64 = std::env::args()
        .nth(1)
        .map(|s| s.parse().unwrap())
        .unwrap_or(1);
    let n = 10000 * scale;
    let mut s = String::new();
    for _ in 0..n {
        s += "x";
    }
    println!("{}", s.len());
}
