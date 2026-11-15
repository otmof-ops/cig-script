// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/words.cig: word count, split text, count in a map, top word.
const parts = [];
for (let i = 0; i < 40000; i++) {
  parts.push(`w${(i * 7919) % 997}`);
}
const text = parts.join(" ");
const counts = new Map();
for (const w of text.split(" ")) {
  if (counts.has(w)) {
    counts.set(w, counts.get(w) + 1);
  } else {
    counts.set(w, 1);
  }
}
let best = "";
let bestn = 0;
for (const w of counts.keys()) {
  if (counts.get(w) > bestn || (counts.get(w) === bestn && w < best)) {
    best = w;
    bestn = counts.get(w);
  }
}
console.log(counts.size, best, bestn);
