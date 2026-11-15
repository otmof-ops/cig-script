-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/regex-ish.cig: string find, replace and split over a megabyte of
-- text (scale in arg 1).
-- cig's s.split(" "): the pieces between the spaces, in order. string.gmatch finds each
-- one in C; no piece here is empty, so "[^ ]+" gives exactly split's pieces.
local function split(s)
  local out = {}
  for piece in string.gmatch(s, "[^ ]+") do
    out[#out + 1] = piece
  end
  return out
end
local scale = tonumber(arg[1] or "1")
local parts = {}
for i = 0, (80000 * scale) - 1 do
  parts[#parts + 1] = "foo-" .. i .. "-bar"
end
local text = table.concat(parts, " ")
-- cig's replace of plain text: "foo" has none of the pattern's magic characters, so
-- string.gsub replaces it as plain text.
local replaced = string.gsub(text, "foo", "FOO")
-- cig's find gives a 0-based index; Lua's a 1-based one.
local found = string.find(text, "bar", 1, true) - 1
local words = split(text)
print(#replaced .. " " .. found .. " " .. #words)
