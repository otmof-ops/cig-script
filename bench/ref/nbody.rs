// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/nbody.cig: n-body, float arithmetic on a small list of maps.
// A body is a struct with named fields: what a Rust program has where cig has a map.
// black_box: the size is not a constant the compiler can fold the work on.
use std::hint::black_box;

const PI: f64 = 3.141592653589793;
const SM: f64 = 4.0 * PI * PI;
const DPY: f64 = 365.24;

struct Body {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    m: f64,
}

fn energy(bodies: &[Body]) -> f64 {
    let mut e = 0.0;
    for i in 0..5 {
        let b = &bodies[i];
        e += 0.5 * b.m * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for j in (i + 1)..5 {
            let c = &bodies[j];
            let dx = b.x - c.x;
            let dy = b.y - c.y;
            let dz = b.z - c.z;
            e -= b.m * c.m / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    e
}

fn advance(bodies: &mut [Body], dt: f64) {
    for i in 0..5 {
        for j in (i + 1)..5 {
            let dx = bodies[i].x - bodies[j].x;
            let dy = bodies[i].y - bodies[j].y;
            let dz = bodies[i].z - bodies[j].z;
            let d2 = dx * dx + dy * dy + dz * dz;
            let mag = dt / (d2 * d2.sqrt());
            let bm = bodies[i].m;
            let cm = bodies[j].m;
            bodies[i].vx -= dx * cm * mag;
            bodies[i].vy -= dy * cm * mag;
            bodies[i].vz -= dz * cm * mag;
            bodies[j].vx += dx * bm * mag;
            bodies[j].vy += dy * bm * mag;
            bodies[j].vz += dz * bm * mag;
        }
    }
    for b in bodies.iter_mut() {
        b.x += dt * b.vx;
        b.y += dt * b.vy;
        b.z += dt * b.vz;
    }
}

fn main() {
    let mut bodies = vec![
        Body {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            vx: 0.0,
            vy: 0.0,
            vz: 0.0,
            m: SM,
        },
        Body {
            x: 4.84143144246472090,
            y: -1.16032004402742839,
            z: -0.103622044471123109,
            vx: 0.00166007664274403694 * DPY,
            vy: 0.00769901118419740425 * DPY,
            vz: -0.0000690460016972063023 * DPY,
            m: 0.000954791938424326609 * SM,
        },
        Body {
            x: 8.34336671824457987,
            y: 4.12479856412430479,
            z: -0.403523417114321381,
            vx: -0.00276742510726862411 * DPY,
            vy: 0.00499852801234917238 * DPY,
            vz: 0.0000230417297573763929 * DPY,
            m: 0.000285885980666130812 * SM,
        },
        Body {
            x: 12.8943695621391310,
            y: -15.1111514016986312,
            z: -0.223307578892655734,
            vx: 0.00296460137564761618 * DPY,
            vy: 0.00237847173959480950 * DPY,
            vz: -0.0000296589568540237556 * DPY,
            m: 0.0000436624404335156298 * SM,
        },
        Body {
            x: 15.3796971148509165,
            y: -25.9193146099879641,
            z: 0.179258772950371181,
            vx: 0.00268067772490389322 * DPY,
            vy: 0.00162824170038242295 * DPY,
            vz: -0.0000951592254519715870 * DPY,
            m: 0.0000515138902046611451 * SM,
        },
    ];
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for b in &bodies {
        px += b.vx * b.m;
        py += b.vy * b.m;
        pz += b.vz * b.m;
    }
    bodies[0].vx = -px / SM;
    bodies[0].vy = -py / SM;
    bodies[0].vz = -pz / SM;
    println!("{:.9}", energy(&bodies));
    let steps: i64 = black_box(20000);
    for _ in 0..steps {
        advance(&mut bodies, 0.01);
    }
    println!("{:.9}", energy(&bodies));
}
