# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/strings.cig: building a long string from pieces.
def main():
    parts = []
    for i in range(60000):
        parts.append(f"item-{i}")
    s = ",".join(parts)
    print(len(s))


main()
