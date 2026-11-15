# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# The fannkuch of bench/compute/fannkuch.cig as Python is usually written: each flip
# and each rotation is one slice assignment, which runs in C; the lists are made by
# list(range(n)) and [0] * n; max() keeps the most flips; and the loop returns at the
# end instead of keeping done and more flags. ../fannkuch.py is the
# statement-for-statement version.
def main():
    n = 8
    perm1 = list(range(n))
    count = [0] * n
    maxflips = 0
    checksum = 0
    permcount = 0
    r = n
    while True:
        while r != 1:
            count[r - 1] = r
            r -= 1
        perm = perm1[:]
        flips = 0
        k = perm[0]
        while k:
            perm[: k + 1] = perm[k::-1]
            flips += 1
            k = perm[0]
        maxflips = max(maxflips, flips)
        checksum += flips if permcount % 2 == 0 else -flips
        while True:
            if r == n:
                print(checksum, maxflips)
                return
            p0 = perm1[0]
            perm1[:r] = perm1[1 : r + 1]
            perm1[r] = p0
            count[r] -= 1
            if count[r] > 0:
                break
            r += 1
        permcount += 1


main()
