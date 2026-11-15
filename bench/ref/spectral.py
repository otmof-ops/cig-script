# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/spectral.cig: spectral norm, float loops and calls.
import math


def a(i, j):
    # cig's / on two ints that divide exactly gives an int, as // does here.
    return 1.0 / ((i + j) * (i + j + 1) // 2 + i + 1)


def mulav(v, n):
    out = []
    for i in range(n):
        s = 0.0
        for j in range(n):
            s += a(i, j) * v[j]
        out.append(s)
    return out


def mulatv(v, n):
    out = []
    for i in range(n):
        s = 0.0
        for j in range(n):
            s += a(j, i) * v[j]
        out.append(s)
    return out


def main():
    n = 100
    u = []
    for i in range(n):
        u.append(1.0)
    v = []
    for k in range(10):
        v = mulatv(mulav(u, n), n)
        u = mulatv(mulav(v, n), n)
    vbv = 0.0
    vv = 0.0
    for i in range(n):
        vbv += u[i] * v[i]
        vv += v[i] * v[i]
    print(round(math.sqrt(vbv / vv), 9))


main()
