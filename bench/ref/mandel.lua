-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/mandel.cig: a tight float loop with an early exit.
local w = 160
local h = 160
local inside = 0
for y = 0, h - 1 do
  for x = 0, w - 1 do
    local cr = 2.0 * x / w - 1.5
    local ci = 2.0 * y / h - 1.0
    local zr = 0.0
    local zi = 0.0
    local k = 0
    local escaped = false
    while k < 50 and not escaped do
      local t = zr * zr - zi * zi + cr
      zi = 2.0 * zr * zi + ci
      zr = t
      if zr * zr + zi * zi > 4.0 then
        escaped = true
      end
      k = k + 1
    end
    if not escaped then
      inside = inside + 1
    end
  end
end
print(inside)
