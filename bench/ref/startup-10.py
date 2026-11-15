# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/startup-10.cig: start-up with ten stdlib calls. Each line is
# the call a Python program makes for the same thing, and the imports are the ones
# those calls need.
import hashlib
import json
import math
import os
import sys
import time

_1 = math.fabs(-1)
_2 = str(1)
_3 = json.dumps({"a": 1})
_4 = "a b".split(" ")
_5 = os.path.basename("/x/y")
_6 = time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime(0))
_7 = hashlib.sha256(b"x").hexdigest()[:8]
_8 = sys.version
_9 = os.environ.get("PATH", "")
_10 = len([])
print("hello")
