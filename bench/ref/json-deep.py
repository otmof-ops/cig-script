# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/json-deep.cig: a six-level nested document, parsed and walked.
import json
import sys


def mkdoc(d, n):
    if d == 0:
        return {"leaf": n}
    kids = []
    for i in range(n):
        kids.append(mkdoc(d - 1, n))
    return {"level": d, "kids": kids}


def walk(x):
    if "leaf" in x:
        return x["leaf"]
    s = 0
    for k in x["kids"]:
        s += walk(k)
    return s + x["level"]


def main():
    scale = int(sys.argv[1]) if len(sys.argv) > 1 else 1
    doc = mkdoc(6, 3)
    body = json.dumps(doc, separators=(",", ":"))
    total = 0
    for i in range(scale):
        back = json.loads(body)
        total += walk(back)
    print(total, len(body))


main()
