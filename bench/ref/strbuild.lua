-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/strbuild.cig: s += piece in a loop, the quadratic trap (scale in arg 1).
local scale = tonumber(arg[1] or "1")
local n = 10000 * scale
local s = ""
for i = 0, n - 1 do
  s = s .. "x"
end
print(#s)
