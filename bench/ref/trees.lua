-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/trees.cig: binary trees, allocation and recursion.
local function make(d)
  if d == 0 then
    return {nil, nil}
  end
  return {make(d - 1), make(d - 1)}
end
local function check(t)
  if t[1] == nil then
    return 1
  end
  return 1 + check(t[1]) + check(t[2])
end
local maxd = 12
print("stretch " .. check(make(maxd + 1)))
local long = make(maxd)
local d = 4
while d <= maxd do
  -- math.pow(2, int) is an int in cig: floor keeps it an integer on Lua 5.4.
  local n = math.floor(2 ^ (maxd - d + 4))
  local c = 0
  for i = 0, n - 1 do
    c = c + check(make(d))
  end
  print(n .. " " .. d .. " " .. c)
  d = d + 2
end
print("long " .. check(long))
