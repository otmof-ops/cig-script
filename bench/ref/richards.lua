-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/richards.cig: a small priority scheduler of tasks, maps and control
-- flow (scale in arg 1).
-- Lua counts from 1 where cig counts from 0: an index here is cig's plus 1.
-- So cig's (i + 3) % 5 is (i + 2) % 5 here, and its (best + 1) % 5 is best % 5 + 1.
local scale = tonumber(arg[1] or "1")
local tasks = {}
for i = 1, 5 do
  tasks[#tasks + 1] = {pri = (i + 2) % 5, work = 0, qlen = 0}
end
local ticks = 10000 * scale
for t = 1, ticks do
  local best = 1
  for i = 2, 5 do
    if tasks[i].pri > tasks[best].pri then
      best = i
    end
  end
  tasks[best].work = tasks[best].work + 1
  tasks[best].qlen = tasks[best].qlen + 1
  local next_i = best % 5 + 1
  if tasks[best].qlen > 3 then
    tasks[best].qlen = tasks[best].qlen - 1
    tasks[next_i].pri = (tasks[next_i].pri + 1) % 5
  end
end
local total = 0
local pri_sum = 0
for i = 1, 5 do
  total = total + tasks[i].work
  pri_sum = pri_sum + tasks[i].pri
end
print(total .. " " .. pri_sum)
