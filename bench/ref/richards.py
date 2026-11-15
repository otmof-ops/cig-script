# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/richards.cig: Richards-like, a small priority scheduler of
# tasks, mixed maps and control flow.
import sys


def main():
    scale = int(sys.argv[1]) if len(sys.argv) > 1 else 1
    tasks = []
    for i in range(5):
        tasks.append({"pri": (i + 3) % 5, "work": 0, "qlen": 0})
    ticks = 10000 * scale
    for t in range(ticks):
        best = 0
        for i in range(1, 5):
            if tasks[i]["pri"] > tasks[best]["pri"]:
                best = i
        tasks[best]["work"] = tasks[best]["work"] + 1
        tasks[best]["qlen"] = tasks[best]["qlen"] + 1
        next_i = (best + 1) % 5
        if tasks[best]["qlen"] > 3:
            tasks[best]["qlen"] = tasks[best]["qlen"] - 1
            tasks[next_i]["pri"] = (tasks[next_i]["pri"] + 1) % 5
    total = 0
    pri_sum = 0
    for i in range(5):
        total += tasks[i]["work"]
        pri_sum += tasks[i]["pri"]
    print(total, pri_sum)


main()
