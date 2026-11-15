// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/regex-ish.cig: string find, replace and split over a megabyte
// of text (5 at 5x).
const scale = parseInt(process.argv[2] ?? "1");
const parts = [];
for (let i = 0; i < 80000 * scale; i++) {
  parts.push(`foo-${i}-bar`);
}
const text = parts.join(" ");
const replaced = text.replaceAll("foo", "FOO");
const found = text.indexOf("bar");
const words = text.split(" ");
console.log(replaced.length, found, words.length);
