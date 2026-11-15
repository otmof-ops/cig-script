# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/csv.cig: CSV written, then read and summed. The cig program
# writes inside a burn, which journals the file so the write can be undone; this is a
# plain write of the same bytes (a header line, then a row a line, each ending in \n).
import csv


def main():
    rows = []
    for i in range(20000):
        rows.append({"series": f"s{i % 5}", "value": i * 1.5})
    with open("bench.csv", "w", newline="") as f:
        out = csv.DictWriter(f, fieldnames=["series", "value"], lineterminator="\n")
        out.writeheader()
        out.writerows(rows)
    with open("bench.csv", newline="") as f:
        back = list(csv.DictReader(f))
    total = 0.0
    for r in back:
        total += float(r["value"])
    print(len(back), total)


main()
