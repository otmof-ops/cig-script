# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/trees.cig: binary trees, allocation and recursion.
def make(d):
    if d == 0:
        return [None, None]
    return [make(d - 1), make(d - 1)]


def check(t):
    if t[0] is None:
        return 1
    return 1 + check(t[0]) + check(t[1])


def main():
    maxd = 12
    print("stretch", check(make(maxd + 1)))
    long = make(maxd)
    d = 4
    while d <= maxd:
        n = 2 ** (maxd - d + 4)
        c = 0
        for i in range(n):
            c += check(make(d))
        print(n, d, c)
        d += 2
    print("long", check(long))


main()
