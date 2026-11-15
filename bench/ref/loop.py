# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/loop.cig: a tight numeric loop.
def main():
    total = 0
    for i in range(400000):
        total += i * i % 7
    print(total)


main()
