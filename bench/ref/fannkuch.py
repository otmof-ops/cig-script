# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/fannkuch.cig: fannkuch-redux, list indexing and swaps.
# The flips and the rotation are loops over indexes, as in the cig program;
# idiom/fannkuch.py does both with slice assignments.
def main():
    n = 8
    perm1 = []
    for i in range(n):
        perm1.append(i)
    count = []
    for i in range(n):
        count.append(0)
    maxflips = 0
    checksum = 0
    permcount = 0
    r = n
    done = False
    while not done:
        while r != 1:
            count[r - 1] = r
            r -= 1
        perm = perm1[:]
        flips = 0
        k = perm[0]
        while k != 0:
            i = 0
            j = k
            while i < j:
                t = perm[i]
                perm[i] = perm[j]
                perm[j] = t
                i += 1
                j -= 1
            flips += 1
            k = perm[0]
        if flips > maxflips:
            maxflips = flips
        if permcount % 2 == 0:
            checksum += flips
        else:
            checksum -= flips
        more = True
        while more:
            if r == n:
                done = True
                more = False
            else:
                p0 = perm1[0]
                i = 0
                while i < r:
                    perm1[i] = perm1[i + 1]
                    i += 1
                perm1[r] = p0
                count[r] = count[r] - 1
                if count[r] > 0:
                    more = False
                else:
                    r += 1
        permcount += 1
    print(checksum, maxflips)


main()
