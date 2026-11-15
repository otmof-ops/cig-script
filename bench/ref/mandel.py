# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
# Mirrors bench/compute/mandel.cig: Mandelbrot, a tight float loop with an early exit.
def main():
    w = 160
    h = 160
    inside = 0
    for y in range(h):
        for x in range(w):
            cr = 2.0 * x / w - 1.5
            ci = 2.0 * y / h - 1.0
            zr = 0.0
            zi = 0.0
            k = 0
            escaped = False
            while k < 50 and not escaped:
                t = zr * zr - zi * zi + cr
                zi = 2.0 * zr * zi + ci
                zr = t
                if zr * zr + zi * zi > 4.0:
                    escaped = True
                k += 1
            if not escaped:
                inside += 1
    print(inside)


main()
