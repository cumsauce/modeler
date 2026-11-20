#!/usr/bin/env python3
"""Generate a normative human walk clip for the `human` body preset.

Run: python3 -I examples/gait_gen.py [--cadence 112] [--out PATH]

(writes examples/human_walk.json with --out examples/human_walk.json; prints
the walk's speed, stride and pelvis bob to stderr)

The legs are built from published normative gait kinematics (Perry &
Burnfield, "Gait Analysis: Normal and Pathological Function", 2nd ed.,
2010; the same knot table `verify animation gait` checks against, in
src/anim/check.rs), not shaped by eye:

1. The thigh and knee angles of the table, applied to this rig's own leg
   segment lengths (forward kinematics from the hip joint), give where the
   ankle is relative to the hip through the cycle. The table is read as a
   periodic Catmull-Rom curve through its knots (no kinks at the knots).
2. Stance follows the three rockers: the heel lands at the table's ankle
   angle (toes up), the foot rolls flat about the heel (to ~12%), stays
   flat while the body passes over it, then from 31% the heel rises about
   the ball (the toes stay flat, MTP up to 40 deg, then roll on the tip) to
   toe-off at 62%. The planted point moves back at exactly the walking
   speed, so the foot never slides; speed and landing spot are a least-
   squares fit of that planted path to the table's path in both double
   supports (where the two legs' table poses have to meet).
3. The pelvis height is whatever keeps the leading stance leg at the table's
   knee angle (handed over from the trailing leg during double support),
   never higher than either stance leg can reach (the rig's knee stops at
   its ~7 deg rest bend), low-passed to a few harmonics of the cycle.
4. Swing follows the table's thigh / knee / ankle, joined to the toe-off
   and heel-strike positions and velocities by a cubic Hermite correction.
   On this rig the table's swing, under the lower pelvis, carries the toe
   tip through the floor, so the ankle is lifted just enough for ~1 cm
   mid-swing toe clearance (3 cm in initial swing, 2 cm heel clearance in
   terminal swing).
5. The foot's channels are low-passed (Gaussian, 1.2% of the cycle) so the
   rocker hand-overs are not corners.
6. Pelvis rotation (+-4 deg, forward on the swing side), obliquity
   (+-4 deg, dropping on the swing side after toe-off), lateral shift
   (+-2 cm toward the stance leg), thorax counter-rotation, a level head,
   and arms swinging opposite the legs (shoulder -12..+16 deg around the
   `stand` pose, elbow 20..45 deg of flexion) are keyed tracks.

The ankles are driven by `ik` (chain 2) and the foot pitch by the ik entry's
`rotate`; the right side mirrors the left at phase 0.5. The clip starts at
30% of the left leg's cycle (mid stance), so no contact sits on the loop seam.
"""

import json
import math
import sys

# ---- normative tables (percent of gait cycle, degrees) ----------------
# Thigh from vertical (+ forward), knee flexion, ankle dorsiflexion (+).
THIGH = [(0, 20), (6, 20), (12, 18), (20, 9), (27, 1), (31, -3), (40, -11),
         (50, -19), (55, -18), (62, -10), (68, 2), (75, 15), (85, 25),
         (92, 23), (100, 20)]
KNEE = [(0, 3), (6, 12), (12, 18), (20, 12), (31, 6), (40, 3), (45, 5),
        (50, 10), (56, 25), (62, 40), (68, 55), (72, 62), (76, 59), (81, 45),
        (87, 25), (93, 10), (100, 3)]
ANKLE = [(0, 0), (6, -5), (12, -2), (20, 2), (31, 5), (40, 8), (47, 10),
         (52, 6), (57, -5), (62, -17), (68, -10), (75, -4), (87, 0), (100, 0)]

# ---- the `human` preset's rest joints (inspect pose of eve_rig) --------
REST = {
    "pelvis": (0.0, 0.870, -0.065),   # hip bone head (the pelvis pivot)
    "hip": (0.101, 0.866, 0.005),     # thigh_l head
    "knee": (0.142, 0.475, 0.037),
    "ankle": (0.181, 0.069, 0.023),
    "ball": (0.195, 0.025, 0.153),
    "toe": (0.197, 0.008, 0.218),     # toe_l tail (the toe tip)
}
KNEE_MIN = 7.5     # the rig's knee can't straighten past its rest bend (~6.7)
KNEE_REACH = 7.0   # IK targets stay within this bend's reach
MTP_MAX = 40.0     # toe (MTP) dorsiflexion at push-off; beyond it the toes roll
TOE_CLEAR = 0.009  # mid-swing toe tip lift over its rest height (normal 1-2 cm)
TOE_OFF = 62.0     # percent of the cycle
FIT_GAP = (12, 40)  # the planted path is fitted to the table at both double
                    # supports (heel strike and push-off), where legs meet
TOE_LIFT = 0.03    # toe tip lift over rest in initial swing
HEEL_CLEAR = 0.02  # terminal-swing heel height before it lands
SMOOTH_GC = 1.2    # foot path low-pass (percent of the cycle, 1 sigma)
PELVIS_HARMONICS = 5  # the pelvis path keeps this many harmonics of the cycle
HEEL_RISE = 3.0    # extra heel rise (deg) around 42% (see q_stance)
SOLE_GAP = 0.001   # the sole's skinned vertices dip a fraction of a mm
TOE_PAD = 0.008    # the toe's sole under its tip joint: the roll pivot
START = 30.0       # the clip starts at this phase of the left leg (mid stance:
                   # no contact event at the loop seam)
HALF_WIDTH = 0.05  # ankle x: 10 cm step width (normal 8-10 cm)
STAND_SHOULDER = {"x": -28, "y": 22, "z": -36}  # examples/human_poses.json
STAND_ELBOW = 14
TAU = 2 * math.pi


def table(t, p):
    """The knot table at percent p, as a periodic Catmull-Rom curve through
    the knots (the motion has no kinks at the knots)."""
    p %= 100.0
    k = t[:-1]  # the last knot repeats the first (100 = 0)
    m = len(k)
    for j in range(m):
        a, b = k[j][0], (k[j + 1][0] if j + 1 < m else 100.0)
        if p <= b:
            p0 = k[j - 1]
            p0x = p0[0] - (100.0 if j == 0 else 0.0)
            p1, p2 = k[j], (k[(j + 1) % m][0] + (100.0 if j + 1 >= m else 0.0), k[(j + 1) % m][1])
            p3k = k[(j + 2) % m]
            p3 = (p3k[0] + (100.0 if j + 2 >= m else 0.0), p3k[1])
            u = (p - a) / (b - a)
            # Non-uniform knots: tangents from neighbouring slopes.
            m1 = (p2[1] - p0[1]) / (p2[0] - p0x) * (b - a)
            m2 = (p3[1] - p1[1]) / (p3[0] - p1[0]) * (b - a)
            h00, h10 = 2 * u**3 - 3 * u**2 + 1, u**3 - 2 * u**2 + u
            h01, h11 = -2 * u**3 + 3 * u**2, u**3 - u**2
            return h00 * p1[1] + h10 * m1 + h01 * p2[1] + h11 * m2
    return t[-1][1]


def sub(a, b):
    return tuple(x - y for x, y in zip(a, b))


def length(v):
    return math.sqrt(sum(x * x for x in v))


def rot2(v, q):
    """Rotate a (forward, up) vector by q radians (+ = toes up)."""
    c, s = math.cos(q), math.sin(q)
    return (v[0] * c - v[1] * s, v[0] * s + v[1] * c)


def euler(v, rx, ry, rz):
    """Rotate a 3D vector: x, then y, then z (degrees, world axes)."""
    x, y, z = v
    a = math.radians(rx)
    y, z = y * math.cos(a) - z * math.sin(a), y * math.sin(a) + z * math.cos(a)
    a = math.radians(ry)
    x, z = x * math.cos(a) + z * math.sin(a), -x * math.sin(a) + z * math.cos(a)
    a = math.radians(rz)
    x, y = x * math.cos(a) - y * math.sin(a), x * math.sin(a) + y * math.cos(a)
    return (x, y, z)


def lowpass(vals, harmonics):
    """Periodic Fourier low-pass of evenly spaced samples."""
    n = len(vals)
    out = [sum(vals) / n] * n
    for k in range(1, harmonics + 1):
        a = sum(v * math.cos(TAU * k * i / n) for i, v in enumerate(vals)) * 2 / n
        b = sum(v * math.sin(TAU * k * i / n) for i, v in enumerate(vals)) * 2 / n
        for i in range(n):
            out[i] += a * math.cos(TAU * k * i / n) + b * math.sin(TAU * k * i / n)
    return out


def smoothstep(u):
    u = min(1.0, max(0.0, u))
    return u * u * (3 - 2 * u)


def build(cadence=112.0, keys=40):
    period = 120.0 / cadence
    hip, knee, ankle = REST["hip"], REST["knee"], REST["ankle"]
    l1, l2 = length(sub(knee, hip)), length(sub(ankle, knee))
    # Sagittal (forward = z, up = y) foot points relative to the ankle.
    ball = (REST["ball"][2] - ankle[2], REST["ball"][1] - ankle[1])
    toe = (REST["toe"][2] - ankle[2], REST["toe"][1] - ankle[1])
    toe_from_ball = (toe[0] - ball[0], toe[1] - ball[1])
    heel = (-0.35 * ball[0], -ankle[1])  # as `gait` places the heel
    shank0 = math.degrees(math.atan2(ankle[2] - knee[2], knee[1] - ankle[1]))
    pelvis = REST["pelvis"]
    hip_off = sub(hip, pelvis)

    def thigh(p):
        return table(THIGH, p)

    def fk(p):
        th, kn = math.radians(thigh(p)), math.radians(table(KNEE, p))
        sh = th - kn
        return (l1 * math.sin(th) + l2 * math.sin(sh),
                -l1 * math.cos(th) - l2 * math.cos(sh))

    def q_norm(p):
        """Foot pitch from rest (deg, + toes up) the table implies."""
        return table(ANKLE, p) + thigh(p) - table(KNEE, p) - shank0

    # Rocker schedule: the heel rocker lasts until the table's foot is flat,
    # the heel rises at 31% (Perry: terminal stance opens with heel rise).
    rock = {"flat": next(p / 10 for p in range(0, 300) if q_norm(p / 10) <= 0),
            "off": 31.0}

    def q_stance(p):
        if p < rock["flat"]:
            return q_norm(p)
        if p < rock["off"]:
            return 0.0
        q_off = q_norm(rock["off"])
        rise = q_norm(p) - q_off * (1 - smoothstep((p - rock["off"]) / (62 - rock["off"])))
        # On the IK leg the shank leans a little further over the planted
        # foot than the table's: the heel rises that much more early in
        # terminal stance, keeping the ankle angle on the table.
        rise -= HEEL_RISE * math.exp(-((p - 42.0) / 6.0) ** 2)
        return min(0.0, rise)

    # Walking speed and where the foot lands: the planted foot (moving back
    # at the walking speed, rolling over heel, flat foot and ball) is fitted
    # by least squares to the table's ankle (heel up: ball) path in both
    # double supports, where the two legs' table poses must meet; mid
    # stance then follows from a foot that cannot slide.
    mid = 25.0
    fit = {"stride": 1.0, "z_mid": fk(mid)[0]}
    def planted_ankle(p, qdeg=None, pivot=None):
        """Ankle (forward, up) of the stance foot, relative to a ground point
        under the hip joint, from the planted heel / ball / toe tip moving
        back, and the foot pitch (rad)."""
        z_flat = fit["z_mid"] - fit["stride"] * (p - mid) / 100.0  # ankle if flat
        q = math.radians(q_stance(p) if qdeg is None else qdeg)
        if pivot is None:
            pivot = "heel" if p < rock["flat"] else "flat" if p < rock["off"] else "ball"
        if pivot == "heel":
            hz = z_flat + heel[0]  # heel stays put while the foot rolls
            r = rot2(heel, q)
            return (hz - r[0], -r[1]), q
        if pivot == "flat":
            return (z_flat, ankle[1]), 0.0
        bz, by = z_flat + ball[0], ankle[1] + ball[1]
        if -math.degrees(q) > MTP_MAX:
            # Rolled onto the toe tip: it stays put, the toes pitch.
            # The pivot is the toe's sole under the tip joint.
            tip = (toe_from_ball[0], toe_from_ball[1] - TOE_PAD)
            tz, ty = bz + tip[0], by + tip[1]
            r = rot2(tip, q + math.radians(MTP_MAX))
            bz, by = tz - r[0], ty - r[1]
        r = rot2(ball, q)
        return (bz - r[0], by - r[1]), q

    def misfit():
        e = 0.0
        for i in range(0, 125):
            p = i / 2.0
            if FIT_GAP[0] < p < FIT_GAP[1]:
                continue
            (az, ay), q = planted_ankle(p)
            f, _ = fk(p)
            if p >= rock["off"]:
                # Heel up: compare the balls of the foot (the planted point).
                az += rot2(ball, q)[0]
                f += rot2(ball, math.radians(q_norm(p)))[0]
            e += (az - f) ** 2
        return e

    def do_fit():
        best = None
        for si in range(0, 81):
            fit["stride"] = 0.9 + si * 0.01
            for zi in range(-40, 41):
                fit["z_mid"] = fk(mid)[0] + zi * 0.0025
                e = misfit()
                if best is None or e < best[0]:
                    best = (e, fit["stride"], fit["z_mid"])
        fit["stride"], fit["z_mid"] = best[1], best[2]

    do_fit()
    stride = fit["stride"]
    speed = stride / period

    def pelvis_state(p):
        u = p / 100.0
        yaw = -4.0 * math.cos(TAU * u)             # left hip forward at left strike
        lst = 4.0 * math.cos(TAU * (u - 0.10))     # left side drops after left toe-off
        shift = 0.02 * math.sin(TAU * (u - 0.03))  # toward the left in left stance
        return yaw, lst, shift

    def hip_world(p, ty, side=1):
        yaw, lst, shift = pelvis_state(p)
        off = (hip_off[0] * side, hip_off[1], hip_off[2])
        r = euler(off, 0.0, yaw, lst)
        return (pelvis[0] + shift + r[0], pelvis[1] + ty + r[1], pelvis[2] + r[2])

    def chord(kn_deg):
        kn = math.radians(kn_deg)
        return math.sqrt(l1 * l1 + l2 * l2 + 2 * l1 * l2 * math.cos(kn))

    def need_y(p, lp, side, kn_deg):
        """Pelvis lift that puts a stance leg (phase lp) at knee kn_deg."""
        (az, ay), _ = planted_ankle(lp)
        h0 = hip_world(p, 0.0, side)
        dx = side * HALF_WIDTH - h0[0]
        dz = az - (h0[2] - hip[2])
        c = chord(kn_deg)
        return ay + math.sqrt(max(c * c - dx * dx - dz * dz, 0.0)) - h0[1]

    # Pelvis height: the leading stance leg (phase 0..50) at the table's
    # knee, never higher than either stance leg can reach.
    n = 200
    ty_curve = [0.0] * n

    def do_ty():
        raw, cap = [], []
        for i in range(n):
            p = 100.0 * i / n
            lead, side = (p, 1) if p < 50 else (p - 50, -1)
            want = need_y(p, lead, side, max(KNEE_MIN, table(KNEE, lead)))
            # Double support: hand the pelvis over from the trailing leg (at
            # its own table knee) to the new leading one over the first 10%.
            w = smoothstep(lead / 10.0)
            if w < 1.0:
                back = need_y(p, lead + 50.0, -side, max(KNEE_MIN, table(KNEE, lead + 50.0)))
                want = w * want + (1 - w) * back
            raw.append(want)
            trail = lead + 50.0
            c = need_y(p, lead, side, KNEE_REACH)
            if trail < 62.0:
                c = min(c, need_y(p, trail, -side, KNEE_REACH))
            cap.append(c)
        tc = [min(a, b) for a, b in zip(raw, cap)]
        for _ in range(8):
            tc = lowpass([min(t, c) for t, c in zip(tc, cap)], PELVIS_HARMONICS)
        # Stay within reach without lowering the whole curve: clip to the
        # cap and round the clipped corners off.
        # Where the smooth curve still pokes over the cap (the V of reach
        # at heel strike), dip it under with smooth bumps, not a clip.
        w = n * 8 // 100
        for _ in range(3):
            over_i = [max(0.0, t - c + 0.0005) for t, c in zip(tc, cap)]
            dip = [max(over_i[(i + d) % n] * (1 - (d / w) ** 2) ** 2 for d in range(-w, w + 1))
                   for i in range(n)]
            tc = [t - 1.2 * d for t, d in zip(tc, dip)]
        over = max(t - c for t, c in zip(tc, cap))
        ty_curve[:] = [t - max(0.0, over) for t in tc]

    def ty_at(p):
        x = (p % 100.0) / 100.0 * n
        i = int(x) % n
        u = x - int(x)
        return ty_curve[i] * (1 - u) + ty_curve[(i + 1) % n] * u

    do_ty()
    stride = fit["stride"]
    speed = stride / period

    def stance_world(p):
        (az, ay), q = planted_ankle(p)
        return (az + hip[2], ay, math.degrees(q))

    def fk_world(p):
        hw = hip_world(p, ty_at(p))
        f, y = fk(p)
        return (hw[2] + f, hw[1] + y, q_norm(p))

    def deriv(fn, p, h=0.05):
        a, b = fn(p - h), fn(p + h)
        return tuple((y - x) / (2 * h) for x, y in zip(a, b))

    # Swing: the table's FK from the hip joint, plus a cubic Hermite
    # correction that leaves toe-off with the stance foot's position and
    # velocity and arrives at the next heel strike likewise.
    span = 100.0 - TOE_OFF
    s0, s1 = stance_world(TOE_OFF - 1e-3), stance_world(1e-3)
    f0, f1 = fk_world(TOE_OFF), fk_world(100.0)
    ds0, ds1 = deriv(stance_world, TOE_OFF - 0.06), deriv(stance_world, 0.06)
    df0, df1 = deriv(fk_world, TOE_OFF + 0.06), deriv(fk_world, 99.94)
    e0 = [a - b for a, b in zip(s0, f0)]
    e1 = [a - b for a, b in zip(s1, f1)]
    m0 = [(a - b) * span for a, b in zip(ds0, df0)]
    m1 = [(a - b) * span for a, b in zip(ds1, df1)]
    # The push-off pitch rate carries into swing; the landing pitch is eased
    # in (the heel rocker starts from rest).
    m1[2] = 0.0

    def raw_swing(p):
        u = (p - TOE_OFF) / span
        h00, h10 = 2 * u**3 - 3 * u**2 + 1, u**3 - 2 * u**2 + u
        h01, h11 = -2 * u**3 + 3 * u**2, u**3 - u**2
        f = fk_world(p)
        return tuple(f[k] + e0[k] * h00 + m0[k] * h10 + e1[k] * h01 + m1[k] * h11
                     for k in range(3))

    def ball_rot(p, q, stance):
        # Toes stay flat on the ground while the heel is up (MTP
        # dorsiflexion); they ease back to the foot's line in early swing.
        if stance:
            return min(MTP_MAX, max(0.0, -q))
        s = smoothstep((p - TOE_OFF) / 14.0)
        q62 = stance_world(TOE_OFF - 1e-3)[2]
        return min(MTP_MAX, max(0.0, -q62)) * (1 - s)

    # Toe clearance: lift the swing ankle just enough, smoothly.
    lifts = []
    for i in range(n):
        p = 100.0 * i / n
        if p < TOE_OFF:
            lifts.append(0.0)
            continue
        z, yv, qd = raw_swing(p)
        q = math.radians(qd)
        br = math.radians(ball_rot(p, qd, False))
        bp = rot2(ball, q)
        tp = rot2(toe_from_ball, q + br)
        toe_y = yv + bp[1] + tp[1] - REST["toe"][1]
        heel_y = yv + rot2(heel, q)[1]
        if p < 75.0:  # the toe leaves the floor at once after toe-off
            want = TOE_LIFT * smoothstep((p - TOE_OFF) / 9.0)
        elif p <= 95.0:
            want = TOE_CLEAR
        else:
            want = 0.004
        need = max(0.0, want - toe_y)
        if p < 96.0:  # the heel skims the floor only as it lands
            need = max(need, HEEL_CLEAR * smoothstep((96.0 - p) / 4.0) - heel_y)
        lifts.append(need)
    win = n // 40
    env = [max(lifts[(i + d) % n] for d in range(-win, win + 1)) for i in range(n)]
    for _ in range(2):
        env = [sum(env[(i + d) % n] for d in range(-win, win + 1)) / (2 * win + 1) for i in range(n)]
    env = [e * smoothstep((100.0 * i / n - TOE_OFF) / 4.0) * smoothstep((100.0 - 100.0 * i / n) / 5.0)
           if 100.0 * i / n >= TOE_OFF else 0.0 for i, e in enumerate(env)]

    def swing_lift(p):
        x = (p % 100.0) / 100.0 * n
        i = int(x) % n
        u = x - int(x)
        return env[i] * (1 - u) + env[(i + 1) % n] * u

    def ankle_target(p):
        """World ankle target, foot pitch (deg) and stance flag, left leg."""
        p %= 100.0
        if p < TOE_OFF:
            z, y, q = stance_world(p)
            return (HALF_WIDTH, y + SOLE_GAP, z), q, True
        z, y, q = raw_swing(p)
        y += swing_lift(p)
        # Keep the target within the leg's reach (the rig's knee stops at
        # its rest bend), pulling it toward the hip joint.
        hw = hip_world(p, ty_at(p))
        d = (HALF_WIDTH - hw[0], y - hw[1], z - hw[2])
        reach = chord(KNEE_REACH) - 0.002
        dl = length(d)
        soft = 0.01  # a smooth clamp: no corner where it starts
        if dl > reach - soft:
            x = (dl - (reach - soft)) / soft
            k = (reach - soft + soft * (1 - math.exp(-x))) / dl
            y, z = hw[1] + d[1] * k, hw[2] + d[2] * k
        return (HALF_WIDTH, y, z), q, False

    def key_t(i):
        return round(period * i / keys, 4)

    # The foot's channels on a fine grid, low-passed a little (a Gaussian of
    # SMOOTH_GC percent of the cycle): the rocker hand-overs at heel strike
    # and toe-off are corners in the ideal path, which no muscle makes.
    # A straight planted path is unchanged by a symmetric kernel.
    fine = 400
    chans = []
    for i in range(fine):
        p = 100.0 * i / fine
        (x, y, z), q, st = ankle_target(p)
        chans.append((y, z, q, ball_rot(p, q, st)))
    sig = SMOOTH_GC / 100.0 * fine
    rad = int(3 * sig)
    kern = [math.exp(-0.5 * (d / sig) ** 2) for d in range(-rad, rad + 1)]
    ks = sum(kern)

    def smooth_at(i, c):
        return sum(kern[d + rad] * chans[(i + d) % fine][c] for d in range(-rad, rad + 1)) / ks

    ik_t, ik_r, ball_k = [], [], []
    for i in range(keys + 1):
        p = (START + 100.0 * i / keys) % 100.0
        j = int(round(p / 100.0 * fine)) % fine
        y, z, q, br = (smooth_at(j, c) for c in range(4))
        ik_t.append([key_t(i), [HALF_WIDTH, round(y, 4), round(z, 4)]])
        ik_r.append([key_t(i), {"x": round(-q, 2)}])
        ball_k.append([key_t(i), {"x": round(-br, 2)}])
    hip_tr, hip_rot, spine, chest, neck = [], [], [], [], []
    sh_k, el_k = [], []
    for i in range(keys + 1):
        p = (START + 100.0 * i / keys) % 100.0
        yaw, lst, shift = pelvis_state(p)
        hip_tr.append([key_t(i), [round(shift, 4), round(ty_at(p), 4), 0.0]])
        hip_rot.append([key_t(i), {"y": round(yaw, 2), "z": round(lst, 2)}])
        # Thorax counter-rotates against the pelvis (world +-4 deg) and stays
        # upright over the tilting pelvis; the head faces forward, level.
        spine.append([key_t(i), {"x": 2.0, "y": round(-yaw, 2), "z": round(-0.6 * lst, 2)}])
        chest.append([key_t(i), {"y": round(-yaw, 2), "z": round(-0.4 * lst, 2)}])
        neck.append([key_t(i), {"x": -2.0, "y": round(-yaw, 2)}])
        # Arms: forward when the same-side leg is back (left leg back at 50%).
        c = math.cos(TAU * (p - 50.0) / 100.0)
        sh = dict(STAND_SHOULDER)
        sh["x"] = round(STAND_SHOULDER["x"] + 2.0 + 14.0 * c, 2)
        sh_k.append([key_t(i), sh])
        el_k.append([key_t(i), {"x": round(STAND_ELBOW - 11.5 + 12.5 * c, 2)}])

    tracks = {
        "hip": {"translate": hip_tr, "rotate": hip_rot},
        "spine": {"rotate": spine},
        "chest": {"rotate": chest},
        "neck": {"rotate": neck},
        "shoulder_l": {"rotate": sh_k},
        "elbow_l": {"rotate": el_k},
        "ball_l": {"rotate": ball_k},
    }
    # Relaxed hands: the `stand` pose's wrist and finger curl, held.
    stand_hand = {"wrist_l": 6}
    for f in ["index", "middle", "ring", "pinky"]:
        for j, v in [(1, 12), (2, 18), (3, 12)]:
            stand_hand[f"{f}_{j}_l"] = v
    for j in (1, 2, 3):
        stand_hand[f"thumb_{j}_l"] = 10
    mirror = {
        "ankle_r": {"from": "ankle_l", "phase": 0.5},
        "ball_r": {"from": "ball_l", "phase": 0.5},
        "shoulder_r": {"from": "shoulder_l", "phase": 0.5},
        "elbow_r": {"from": "elbow_l", "phase": 0.5},
    }
    for b, v in stand_hand.items():
        tracks[b] = {"rotate": [[0, {"x": v}]]}
        mirror[b[:-2] + "_r"] = {"from": b, "phase": 0.5}
    clip = {
        "type": "clip",
        "name": "walk",
        "skeleton": "eve_rig",
        "duration": round(period, 4),
        "tracks": tracks,
        "ik": {"ankle_l": {"target": ik_t, "pole": [HALF_WIDTH + 0.03, 0.5, 1.0],
                           "rotate": ik_r}},
        "mirror": mirror,
        "events": [[round(period * (100.0 - START) / 100.0, 4), "footstep_l"],
                   [round(period * ((150.0 - START) % 100.0) / 100.0, 4), "footstep_r"]],
    }
    meta = {"speed": round(speed, 4), "stride": round(stride, 4),
            "period": round(period, 4), "foot_flat": rock["flat"], "heel_off": rock["off"],
            "pelvis_bob_cm": round(100 * (max(ty_curve) - min(ty_curve)), 2),
            "max_swing_lift_cm": round(100 * max(env), 2)}
    return [{"type": "body", "name": "eve", "preset": "human"}, clip], meta


def main():
    args = sys.argv[1:]
    cadence = 112.0
    out = None
    if "--cadence" in args:
        cadence = float(args[args.index("--cadence") + 1])
    if "--out" in args:
        out = args[args.index("--out") + 1]
    items, meta = build(cadence)
    text = json.dumps(items, indent=1)
    if out:
        with open(out, "w") as fh:
            fh.write(text + "\n")
    else:
        print(text)
    print(json.dumps(meta), file=sys.stderr)


if __name__ == "__main__":
    main()
