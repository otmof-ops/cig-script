-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/words.cig: word count, split text, count in a map, top word.
-- cig's s.split(" "): the pieces between the spaces, in order. string.gmatch finds each
-- one in C; no piece here is empty, so "[^ ]+" gives exactly split's pieces.
local function split(s)
  local out = {}
  for piece in string.gmatch(s, "[^ ]+") do
    out[#out + 1] = piece
  end
  return out
end
local parts = {}
for i = 0, 40000 - 1 do
  parts[#parts + 1] = "w" .. (i * 7919) % 997
end
local text = table.concat(parts, " ")
local counts = {}
local words = split(text)
for i = 1, #words do
  local w = words[i]
  if counts[w] ~= nil then
    counts[w] = counts[w] + 1
  else
    counts[w] = 1
  end
end
-- The top word does not depend on the order the keys come in (the most uses, then the
-- smallest word), so pairs() serves; a Lua table does not keep its size, so the keys
-- are counted on the way.
local best = ""
local bestn = 0
local keys = 0
for w, n in pairs(counts) do
  keys = keys + 1
  if n > bestn or (n == bestn and w < best) then
    best = w
    bestn = n
  end
end
print(keys .. " " .. best .. " " .. bestn)
