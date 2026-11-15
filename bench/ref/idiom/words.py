# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# The word count of bench/compute/words.cig as Python is usually written: the text made
# by a join over a generator, the words counted by collections.Counter (its counting
# loop runs in C), and the top word found by min() with a key that ranks each word by
# its count, then by the word. ../words.py is the statement-for-statement version.
from collections import Counter


def main():
    text = " ".join(f"w{(i * 7919) % 997}" for i in range(40000))
    counts = Counter(text.split(" "))
    best = min(counts, key=lambda w: (-counts[w], w))
    print(len(counts), best, counts[best])


main()
