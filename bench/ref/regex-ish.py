# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/regex-ish.cig: string find, replace and split over a megabyte
# of text (5 at 5x).
import sys


def main():
    scale = int(sys.argv[1]) if len(sys.argv) > 1 else 1
    parts = []
    for i in range(80000 * scale):
        parts.append(f"foo-{i}-bar")
    text = " ".join(parts)
    replaced = text.replace("foo", "FOO")
    found = text.find("bar")
    words = text.split(" ")
    print(len(replaced), found, len(words))


main()
