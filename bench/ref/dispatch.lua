-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/dispatch.cig: functions kept in a map, called by key.
local ops = {
  add = function(a, b) return a + b end,
  sub = function(a, b) return a - b end,
  mul = function(a, b) return a * b % 1000003 end,
}
local names = {"add", "sub", "mul"}
local acc = 1
for i = 0, 150000 - 1 do
  local f = ops[names[i % 3 + 1]]
  acc = (f(acc, i) % 1000003 + 1000003) % 1000003
end
print(acc)
