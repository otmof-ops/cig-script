-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/deltablue.cig: a constraint chain, DeltaBlue-like (scale in arg 1).
-- Lua counts from 1 where cig counts from 0: an index here is cig's plus 1.
-- So cig's i * 7 + 1, with its i one less than the index here, is i * 7 - 6.
local scale = tonumber(arg[1] or "1")
local n = 20 * scale
local iters = 500
local MOD = 1000003
local vars = {}
for i = 1, n do
  vars[#vars + 1] = 0
end
local changes = 0
for k = 0, iters - 1 do
  vars[1] = k
  for i = 2, n do
    local prev = vars[i - 1]
    local want = (prev * 2 + i * 7 - 6) % MOD
    if vars[i] ~= want then
      vars[i] = want
      changes = changes + 1
    end
  end
end
local total = 0
for i = 1, #vars do
  local v = vars[i]
  total = (total + v) % MOD
end
print(total .. " " .. changes)
