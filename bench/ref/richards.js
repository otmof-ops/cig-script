// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/richards.cig: Richards-like, a small priority scheduler of
// tasks, mixed maps and control flow.
const scale = parseInt(process.argv[2] ?? "1");
const tasks = [];
for (let i = 0; i < 5; i++) {
  tasks.push({ pri: (i + 3) % 5, work: 0, qlen: 0 });
}
const ticks = 10000 * scale;
for (let t = 0; t < ticks; t++) {
  let best = 0;
  for (let i = 1; i < 5; i++) {
    if (tasks[i].pri > tasks[best].pri) {
      best = i;
    }
  }
  tasks[best].work = tasks[best].work + 1;
  tasks[best].qlen = tasks[best].qlen + 1;
  const next_i = (best + 1) % 5;
  if (tasks[best].qlen > 3) {
    tasks[best].qlen = tasks[best].qlen - 1;
    tasks[next_i].pri = (tasks[next_i].pri + 1) % 5;
  }
}
let total = 0;
let pri_sum = 0;
for (let i = 0; i < 5; i++) {
  total += tasks[i].work;
  pri_sum += tasks[i].pri;
}
console.log(total, pri_sum);
