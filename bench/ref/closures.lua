-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/closures.cig: closures made in a loop and called.
local function counter(start)
  local n = start
  return function() return n + 1 end
end
local total = 0
for i = 0, 100000 - 1 do
  local f = counter(i)
  total = total + f()
end
print(total)
