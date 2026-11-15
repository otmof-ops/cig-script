# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/maps.cig: maps, inserts and lookups.
def main():
    m = {}
    for i in range(50000):
        m[f"k{i}"] = i
    hits = 0
    for i in range(50000):
        if f"k{i * 2}" in m:
            hits += 1
    print(hits)


main()
