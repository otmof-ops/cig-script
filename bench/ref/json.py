# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/json.cig: JSON out and back in.
import json


def main():
    rows = []
    for i in range(20000):
        rows.append({"id": i, "name": f"n{i}", "tags": ["a", "b"], "ok": i % 3 == 0})
    body = json.dumps(rows, separators=(",", ":"))
    back = json.loads(body)
    print(len(back), len(body))


main()
