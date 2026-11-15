# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/dispatch.cig: functions kept in a map, called by key.
def main():
    ops = {
        "add": lambda a, b: a + b,
        "sub": lambda a, b: a - b,
        "mul": lambda a, b: a * b % 1000003,
    }
    names = ["add", "sub", "mul"]
    acc = 1
    for i in range(150000):
        f = ops[names[i % 3]]
        acc = (f(acc, i) % 1000003 + 1000003) % 1000003
    print(acc)


main()
