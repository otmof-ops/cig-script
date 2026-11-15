-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/loop.cig: a tight numeric loop.
local total = 0
for i = 0, 400000 - 1 do
  total = total + i * i % 7
end
print(total)
