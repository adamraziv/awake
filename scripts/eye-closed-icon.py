#!/usr/bin/env python3
"""Writes crates/awake-app/assets/eye-closed.pdf, the idle menu bar icon.

SF Symbols has `eye` but no fully closed eye, so this draws one: a lid curve with five lashes in
the same 21 x 13 pt box and roughly the same stroke weight as the `eye` symbol, which is the
active icon. Run it from anywhere; it writes next to the crate. PDF y points up.
"""
import math, pathlib
W, H = 21.0, 13.0
P0, C1, C2, P3 = (1.5, 8.6), (6.2, 3.9), (14.7, 3.9), (19.4, 8.6)
STROKE, LASH = 1.25, 2.3
ts = [0.2, 0.35, 0.5, 0.65, 0.8]
def bez(t):
    u = 1 - t
    return tuple(u**3*P0[i] + 3*u*u*t*C1[i] + 3*u*t*t*C2[i] + t**3*P3[i] for i in range(2))
def tangent(t):
    u = 1 - t
    return tuple(3*u*u*(C1[i]-P0[i]) + 6*u*t*(C2[i]-C1[i]) + 3*t*t*(P3[i]-C2[i]) for i in range(2))
ops = [f"{STROKE:.2f} w 1 J 1 j 0 g",
       f"{P0[0]} {P0[1]} m {C1[0]} {C1[1]} {C2[0]} {C2[1]} {P3[0]} {P3[1]} c S"]
for t in ts:
    (x, y), (dx, dy) = bez(t), tangent(t)
    n = math.hypot(dx, dy); nx, ny = dy / n, -dx / n   # normal pointing down/out
    if ny > 0: nx, ny = -nx, -ny
    ops.append(f"{x:.3f} {y:.3f} m {x + nx*LASH:.3f} {y + ny*LASH:.3f} l S")
content = "\n".join(ops).encode()
objs = [b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {W:g} {H:g}] /Contents 4 0 R /Resources << >> >>".encode(),
        b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream"]
out = bytearray(b"%PDF-1.4\n"); offs = []
for i, o in enumerate(objs, 1):
    offs.append(len(out)); out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
xref = len(out)
out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1) + b"".join(b"%010d 00000 n \n" % x for x in offs)
out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
dest = pathlib.Path(__file__).resolve().parent.parent / "crates/awake-app/assets/eye-closed.pdf"
dest.write_bytes(bytes(out))
print(f"wrote {dest} ({len(out)} bytes)")
