# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/sieve.cig: sieve of Eratosthenes, index writes into a big list.
# The list is built and counted in loops, as the cig program does; idiom/sieve.py
# builds it with a list repeat and counts it with sum().
def main():
    n = 300000
    flags = []
    for i in range(n + 1):
        flags.append(True)
    flags[0] = False
    flags[1] = False
    p = 2
    while p * p <= n:
        if flags[p]:
            m = p * p
            while m <= n:
                flags[m] = False
                m += p
        p += 1
    c = 0
    for b in flags:
        if b:
            c += 1
    print(c)


main()
