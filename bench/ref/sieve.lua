-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/sieve.cig: Sieve of Eratosthenes, index writes into a big list.
-- The number i is at index i; 0, which is not prime, has no entry (cig's list has one,
-- set to false).
local n = 300000
local is = {}
for i = 1, n do
  is[i] = true
end
is[1] = false
local p = 2
while p * p <= n do
  if is[p] then
    local m = p * p
    while m <= n do
      is[m] = false
      m = m + p
    end
  end
  p = p + 1
end
local c = 0
for i = 1, n do
  if is[i] then
    c = c + 1
  end
end
print(c)
