# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/words.cig: word count, split text, count in a map, top word.
def main():
    parts = []
    for i in range(40000):
        parts.append(f"w{(i * 7919) % 997}")
    text = " ".join(parts)
    counts = {}
    for w in text.split(" "):
        if w in counts:
            counts[w] = counts[w] + 1
        else:
            counts[w] = 1
    best = ""
    bestn = 0
    for w in counts.keys():
        if counts[w] > bestn or (counts[w] == bestn and w < best):
            best = w
            bestn = counts[w]
    print(len(counts), best, bestn)


main()
