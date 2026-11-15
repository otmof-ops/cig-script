-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/maps.cig: maps, inserts and lookups.
local m = {}
for i = 0, 50000 - 1 do
  m["k" .. i] = i
end
local hits = 0
for i = 0, 50000 - 1 do
  if m["k" .. i * 2] ~= nil then
    hits = hits + 1
  end
end
print(hits)
