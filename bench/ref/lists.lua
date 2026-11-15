-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/lists.cig: lists, sort, filter and map.
-- cig's xs.filter(f): a new list of the items f keeps.
local function filter(xs, f)
  local out = {}
  for i = 1, #xs do
    local x = xs[i]
    if f(x) then
      out[#out + 1] = x
    end
  end
  return out
end
-- cig's xs.map(f): a new list of f of each item.
local function map(xs, f)
  local out = {}
  for i = 1, #xs do
    out[#out + 1] = f(xs[i])
  end
  return out
end
local xs = {}
for i = 0, 60000 - 1 do
  xs[#xs + 1] = (i * 7919) % 10007
end
-- cig's xs.sort() gives a sorted copy, made in native code. Lua has no native copy,
-- and xs is not used again, so it is sorted where it is, as a Lua programmer would.
table.sort(xs)
local sorted = xs
local evens = filter(sorted, function(x) return x % 2 == 0 end)
local doubled = map(evens, function(x) return x * 2 end)
print(#doubled .. " " .. doubled[1] .. " " .. doubled[#doubled])
