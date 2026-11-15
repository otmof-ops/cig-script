# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/strbuild.cig: string building, s += piece.
import sys


def main():
    scale = int(sys.argv[1]) if len(sys.argv) > 1 else 1
    n = 10000 * scale
    s = ""
    for i in range(n):
        s += "x"
    print(len(s))


main()
