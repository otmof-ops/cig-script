// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/nbody.cig: n-body, float arithmetic on a small list of maps.
// A body is an object with named fields, as it is a map with named fields in cig.
const PI = 3.141592653589793;
const SM = 4.0 * PI * PI;
const DPY = 365.24;
const bodies = [
  { x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, m: SM },
  { x: 4.84143144246472090, y: -1.16032004402742839, z: -0.103622044471123109, vx: 0.00166007664274403694 * DPY, vy: 0.00769901118419740425 * DPY, vz: -0.0000690460016972063023 * DPY, m: 0.000954791938424326609 * SM },
  { x: 8.34336671824457987, y: 4.12479856412430479, z: -0.403523417114321381, vx: -0.00276742510726862411 * DPY, vy: 0.00499852801234917238 * DPY, vz: 0.0000230417297573763929 * DPY, m: 0.000285885980666130812 * SM },
  { x: 12.8943695621391310, y: -15.1111514016986312, z: -0.223307578892655734, vx: 0.00296460137564761618 * DPY, vy: 0.00237847173959480950 * DPY, vz: -0.0000296589568540237556 * DPY, m: 0.0000436624404335156298 * SM },
  { x: 15.3796971148509165, y: -25.9193146099879641, z: 0.179258772950371181, vx: 0.00268067772490389322 * DPY, vy: 0.00162824170038242295 * DPY, vz: -0.0000951592254519715870 * DPY, m: 0.0000515138902046611451 * SM },
];
function energy() {
  let e = 0.0;
  for (let i = 0; i < 5; i++) {
    const b = bodies[i];
    e += 0.5 * b.m * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
    for (let j = i + 1; j < 5; j++) {
      const c = bodies[j];
      const dx = b.x - c.x;
      const dy = b.y - c.y;
      const dz = b.z - c.z;
      e -= (b.m * c.m) / Math.sqrt(dx * dx + dy * dy + dz * dz);
    }
  }
  return e;
}
function advance(dt) {
  for (let i = 0; i < 5; i++) {
    const b = bodies[i];
    for (let j = i + 1; j < 5; j++) {
      const c = bodies[j];
      const dx = b.x - c.x;
      const dy = b.y - c.y;
      const dz = b.z - c.z;
      const d2 = dx * dx + dy * dy + dz * dz;
      const mag = dt / (d2 * Math.sqrt(d2));
      b.vx = b.vx - dx * c.m * mag;
      b.vy = b.vy - dy * c.m * mag;
      b.vz = b.vz - dz * c.m * mag;
      c.vx = c.vx + dx * b.m * mag;
      c.vy = c.vy + dy * b.m * mag;
      c.vz = c.vz + dz * b.m * mag;
    }
  }
  for (const b of bodies) {
    b.x = b.x + dt * b.vx;
    b.y = b.y + dt * b.vy;
    b.z = b.z + dt * b.vz;
  }
}
let px = 0.0;
let py = 0.0;
let pz = 0.0;
for (const b of bodies) {
  px += b.vx * b.m;
  py += b.vy * b.m;
  pz += b.vz * b.m;
}
bodies[0].vx = -px / SM;
bodies[0].vy = -py / SM;
bodies[0].vz = -pz / SM;
// math.round(x, 9): JavaScript has no rounding to a number of places.
const round9 = (x) => Math.round(x * 1e9) / 1e9;
console.log(round9(energy()));
for (let s = 0; s < 20000; s++) {
  advance(0.01);
}
console.log(round9(energy()));
