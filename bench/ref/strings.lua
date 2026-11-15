-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/strings.cig: building a long string from pieces (join is table.concat).
local parts = {}
for i = 0, 60000 - 1 do
  parts[#parts + 1] = "item-" .. i
end
local s = table.concat(parts, ",")
print(#s)
