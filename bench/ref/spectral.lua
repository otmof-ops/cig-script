-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/spectral.cig: spectral norm, float loops and calls.
-- Lua counts from 1 where cig counts from 0: an index here is cig's plus 1.

-- cig's float display (value.rs format_float): integral floats keep ".0", others print
-- the shortest form that reads back to the same float. Only finite values under 1e16
-- (no exponent form) reach it here.
local function format_float(f)
  if f == math.floor(f) then
    return string.format("%.1f", f)
  end
  for p = 15, 17 do
    local s = string.format("%." .. p .. "g", f)
    if tonumber(s) == f then
      return s
    end
  end
end

-- cig's math.round(x, places): (x * 10^places).round() / 10^places, half away from zero.
local function round(x, places)
  local factor = 10 ^ places
  local y = x * factor
  if y >= 0 then
    y = math.floor(y + 0.5)
  else
    y = -math.floor(-y + 0.5)
  end
  return y / factor
end

-- i and j count from 1 here and from 0 in the cig program: the same entry of the matrix.
local function a(i, j)
  return 1.0 / ((i + j - 2) * (i + j - 1) / 2 + i)
end
local function mulav(v, n)
  local out = {}
  for i = 1, n do
    local s = 0.0
    for j = 1, n do
      s = s + a(i, j) * v[j]
    end
    out[#out + 1] = s
  end
  return out
end
local function mulatv(v, n)
  local out = {}
  for i = 1, n do
    local s = 0.0
    for j = 1, n do
      s = s + a(j, i) * v[j]
    end
    out[#out + 1] = s
  end
  return out
end
local n = 100
local u = {}
for i = 1, n do
  u[#u + 1] = 1.0
end
local v = {}
for k = 1, 10 do
  v = mulatv(mulav(u, n), n)
  u = mulatv(mulav(v, n), n)
end
local vbv = 0.0
local vv = 0.0
for i = 1, n do
  vbv = vbv + u[i] * v[i]
  vv = vv + v[i] * v[i]
end
print(format_float(round(math.sqrt(vbv / vv), 9)))
