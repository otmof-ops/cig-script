-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
-- SPDX-License-Identifier: FSL-1.1-ALv2
-- Mirrors bench/compute/nbody.cig: n-body, float arithmetic on a small list of maps.
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

local PI = 3.141592653589793
local SM = 4.0 * PI * PI
local DPY = 365.24
local bodies = {
  {x = 0.0, y = 0.0, z = 0.0, vx = 0.0, vy = 0.0, vz = 0.0, m = SM},
  {x = 4.84143144246472090, y = -1.16032004402742839, z = -0.103622044471123109, vx = 0.00166007664274403694 * DPY, vy = 0.00769901118419740425 * DPY, vz = -0.0000690460016972063023 * DPY, m = 0.000954791938424326609 * SM},
  {x = 8.34336671824457987, y = 4.12479856412430479, z = -0.403523417114321381, vx = -0.00276742510726862411 * DPY, vy = 0.00499852801234917238 * DPY, vz = 0.0000230417297573763929 * DPY, m = 0.000285885980666130812 * SM},
  {x = 12.8943695621391310, y = -15.1111514016986312, z = -0.223307578892655734, vx = 0.00296460137564761618 * DPY, vy = 0.00237847173959480950 * DPY, vz = -0.0000296589568540237556 * DPY, m = 0.0000436624404335156298 * SM},
  {x = 15.3796971148509165, y = -25.9193146099879641, z = 0.179258772950371181, vx = 0.00268067772490389322 * DPY, vy = 0.00162824170038242295 * DPY, vz = -0.0000951592254519715870 * DPY, m = 0.0000515138902046611451 * SM},
}
local function energy()
  local e = 0.0
  for i = 1, 5 do
    local b = bodies[i]
    e = e + 0.5 * b.m * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz)
    for j = i + 1, 5 do
      local c = bodies[j]
      local dx = b.x - c.x
      local dy = b.y - c.y
      local dz = b.z - c.z
      e = e - b.m * c.m / math.sqrt(dx * dx + dy * dy + dz * dz)
    end
  end
  return e
end
local function advance(dt)
  for i = 1, 5 do
    local b = bodies[i]
    for j = i + 1, 5 do
      local c = bodies[j]
      local dx = b.x - c.x
      local dy = b.y - c.y
      local dz = b.z - c.z
      local d2 = dx * dx + dy * dy + dz * dz
      local mag = dt / (d2 * math.sqrt(d2))
      b.vx = b.vx - dx * c.m * mag
      b.vy = b.vy - dy * c.m * mag
      b.vz = b.vz - dz * c.m * mag
      c.vx = c.vx + dx * b.m * mag
      c.vy = c.vy + dy * b.m * mag
      c.vz = c.vz + dz * b.m * mag
    end
  end
  for i = 1, #bodies do
    local b = bodies[i]
    b.x = b.x + dt * b.vx
    b.y = b.y + dt * b.vy
    b.z = b.z + dt * b.vz
  end
end
local px = 0.0
local py = 0.0
local pz = 0.0
for i = 1, #bodies do
  local b = bodies[i]
  px = px + b.vx * b.m
  py = py + b.vy * b.m
  pz = pz + b.vz * b.m
end
bodies[1].vx = -px / SM
bodies[1].vy = -py / SM
bodies[1].vz = -pz / SM
print(format_float(round(energy(), 9)))
for s = 1, 20000 do
  advance(0.01)
end
print(format_float(round(energy(), 9)))
