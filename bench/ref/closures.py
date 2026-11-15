# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/closures.cig: closures made in a loop and called.
def counter(start):
    n = start
    return lambda: n + 1


def main():
    total = 0
    for i in range(100000):
        f = counter(i)
        total += f()
    print(total)


main()
