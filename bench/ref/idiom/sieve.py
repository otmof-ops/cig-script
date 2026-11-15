# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# The sieve of bench/compute/sieve.cig as Python is usually written: the list made
# by a list repeat and counted by sum(), both of which run in C. ../sieve.py is the
# statement-for-statement version.
def main():
    n = 300000
    flags = [True] * (n + 1)
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
    print(sum(flags))


main()
