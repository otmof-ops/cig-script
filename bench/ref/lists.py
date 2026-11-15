# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/lists.cig: lists, sort, filter and map.
def main():
    xs = []
    for i in range(60000):
        xs.append((i * 7919) % 10007)
    sorted_xs = sorted(xs)
    evens = [x for x in sorted_xs if x % 2 == 0]
    doubled = [x * 2 for x in evens]
    print(len(doubled), doubled[0], doubled[len(doubled) - 1])


main()
