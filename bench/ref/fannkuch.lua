-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/fannkuch.cig: fannkuch-redux, list indexing and swaps.
-- Lua counts from 1 where cig counts from 0: an index here is cig's plus 1.
local n = 8
local perm1 = {}
for i = 0, n - 1 do
  perm1[#perm1 + 1] = i
end
local count = {}
for i = 0, n - 1 do
  count[#count + 1] = 0
end
local maxflips = 0
local checksum = 0
local permcount = 0
local r = n
local done = false
while not done do
  while r ~= 1 do
    count[r] = r
    r = r - 1
  end
  -- perm1.slice(0): a fresh copy of the whole list.
  local perm = {}
  for i = 1, #perm1 do
    perm[i] = perm1[i]
  end
  local flips = 0
  local k = perm[1]
  while k ~= 0 do
    local i = 1
    local j = k + 1
    while i < j do
      local t = perm[i]
      perm[i] = perm[j]
      perm[j] = t
      i = i + 1
      j = j - 1
    end
    flips = flips + 1
    k = perm[1]
  end
  if flips > maxflips then
    maxflips = flips
  end
  if permcount % 2 == 0 then
    checksum = checksum + flips
  else
    checksum = checksum - flips
  end
  local more = true
  while more do
    if r == n then
      done = true
      more = false
    else
      local p0 = perm1[1]
      local i = 1
      while i <= r do
        perm1[i] = perm1[i + 1]
        i = i + 1
      end
      perm1[r + 1] = p0
      count[r + 1] = count[r + 1] - 1
      if count[r + 1] > 0 then
        more = false
      else
        r = r + 1
      end
    end
  end
  permcount = permcount + 1
end
print(checksum .. " " .. maxflips)
