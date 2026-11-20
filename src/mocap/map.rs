//! Bone mapping by name: BVH joint names in the common conventions (CMU /
//! Motionbuilder `LeftUpLeg`, Mixamo `mixamorig:LeftUpLeg`, 3ds Max biped
//! `Bip01 L Thigh`, CMU ASF `lfemur`, lowercase / underscore variants,
//! fingers) onto modeler's joint-named bones (`hip`, `thigh_l`, `knee_l`,
//! `shoulder_l` = upper arm, `elbow_l` = forearm, `wrist_l` = hand, ...).

use super::bvh::Bvh;

/// Splits a name into lowercase words: at non-alphanumerics and camel-case
/// boundaries (`LeftUpLeg` -> left, up, leg; `LThumb` -> l, thumb); digits
/// stay on the word before them (`Spine1`).
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let cs: Vec<char> = name.chars().collect();
    let mut cur = String::new();
    for (i, &c) in cs.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_uppercase() && !cur.is_empty() {
            let prev = cs[i - 1];
            let next_lower = cs.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What a core name means: a sided base, an unsided one, or a spine / neck
/// segment (resolved by position in the chain).
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Sided(String),
    Plain(String),
    Spine,
    Neck,
}

fn finger(core: &str) -> Option<String> {
    let core = core
        .strip_prefix("hand")
        .or_else(|| {
            core.strip_prefix("finger")
                .filter(|r| !r.starts_with(char::is_numeric))
        })
        .unwrap_or(core);
    // 3ds Max: Finger0 / Finger01 / Finger02 (0 thumb .. 4 pinky).
    if let Some(d) = core.strip_prefix("finger") {
        let ds: Vec<u32> = d.chars().filter_map(|c| c.to_digit(10)).collect();
        if ds.is_empty() || ds.len() != d.len() {
            return None;
        }
        let f = ["thumb", "index", "middle", "ring", "pinky"].get(ds[0] as usize)?;
        let seg = ds.get(1).map_or(1, |s| s + 1);
        return (seg <= 3).then(|| format!("{f}_{seg}"));
    }
    for (pre, f) in [
        ("thumb", "thumb"),
        ("index", "index"),
        ("middle", "middle"),
        ("mid", "middle"),
        ("ring", "ring"),
        ("pinky", "pinky"),
        ("pinkie", "pinky"),
        ("little", "pinky"),
    ] {
        if let Some(rest) = core.strip_prefix(pre) {
            let rest = rest.strip_prefix("finger").unwrap_or(rest);
            if rest.is_empty() {
                return Some(format!("{f}_1"));
            }
            let d: u32 = rest.parse().ok()?;
            return (1..=3).contains(&d).then(|| format!("{f}_{d}"));
        }
    }
    None
}

fn kind_of(core: &str, sided: bool) -> Option<Kind> {
    let s = |b: &str| Some(Kind::Sided(b.into()));
    if sided {
        return match core {
            "hip" | "upleg" | "upperleg" | "thigh" | "femur" => s("thigh"),
            "leg" | "lowerleg" | "calf" | "shin" | "knee" | "tibia" | "crus" => s("knee"),
            "foot" | "ankle" => s("ankle"),
            "toebase" | "toe" | "toes" | "ball" | "toe0" | "toes0" => s("ball"),
            "toeend" | "toebaseend" | "toesend" | "toetip" | "toe0nub" | "toenub" => s("toe"),
            "clavicle" | "shoulder" | "collar" | "collarbone" => s("clavicle"),
            "arm" | "uparm" | "upperarm" | "humerus" => s("shoulder"),
            "forearm" | "lowerarm" | "elbow" | "radius" => s("elbow"),
            "hand" | "wrist" => s("wrist"),
            _ => finger(core).map(Kind::Sided),
        };
    }
    match core {
        "hips" | "hip" | "pelvis" => Some(Kind::Plain("hip".into())),
        "head" => Some(Kind::Plain("head".into())),
        "jaw" => Some(Kind::Plain("jaw".into())),
        "lowerback" | "upperback" | "chest" | "upperchest" | "thorax" | "abdomen" | "torso"
        | "back" => Some(Kind::Spine),
        c if c.starts_with("spine") && c[5..].chars().all(|d| d.is_ascii_digit()) => {
            Some(Kind::Spine)
        }
        c if c.starts_with("neck") && c[4..].chars().all(|d| d.is_ascii_digit()) => {
            Some(Kind::Neck)
        }
        "lowerneck" | "upperneck" => Some(Kind::Neck),
        _ => None,
    }
}

/// A joint name's side ('l' / 'r') and kind.
fn classify(name: &str) -> Option<(Option<char>, Kind)> {
    let after_ns = name.rsplit(':').next().unwrap_or(name);
    let mut w = words(after_ns);
    while let Some(first) = w.first() {
        let drop = first == "mixamorig"
            || first.starts_with("character")
            || first == "rig"
            || first == "def"
            || first == "org"
            || (first.starts_with("bip") && first[3..].chars().all(|c| c.is_ascii_digit()));
        if !drop || w.len() == 1 {
            break;
        }
        w.remove(0);
    }
    let mut side = None;
    let is = |t: &str, l: &str, s: &str| t == l || t == s;
    if let Some(t) = w.first().cloned() {
        if is(&t, "left", "l") {
            side = Some('l');
            w.remove(0);
        } else if is(&t, "right", "r") {
            side = Some('r');
            w.remove(0);
        }
    }
    if side.is_none()
        && let Some(t) = w.last().cloned()
    {
        if is(&t, "left", "l") {
            side = Some('l');
            w.pop();
        } else if is(&t, "right", "r") {
            side = Some('r');
            w.pop();
        }
    }
    let core: String = w.concat();
    if core.is_empty() {
        return None;
    }
    if let Some(k) = kind_of(&core, side.is_some()) {
        return Some((side, k));
    }
    // CMU ASF style: `lfemur`, `rhumerus` (a side letter glued on).
    if side.is_none() && core.len() > 1 {
        let (c, rest) = core.split_at(1);
        if (c == "l" || c == "r")
            && let Some(k) = kind_of(rest, true)
        {
            return Some((c.chars().next(), k));
        }
    }
    None
}

/// Target bone -> source joint, by name: exact names first (ignoring case),
/// then the conventions above. Spine segments between the hips and the neck
/// go to `spine` (the first) and `chest` (the last); the first neck segment
/// to `neck`. A target bone takes the source joint nearest the root.
pub fn auto(bvh: &Bvh, has: &dyn Fn(&str) -> bool) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut used = vec![false; bvh.joints.len()];
    let taken = |out: &[(String, usize)], n: &str| out.iter().any(|(t, _)| t == n);
    for (j, jt) in bvh.joints.iter().enumerate() {
        let lower = jt.name.to_lowercase();
        // Spine and neck segments go by their place in the chain.
        let chain = matches!(classify(&jt.name), Some((_, Kind::Spine | Kind::Neck)));
        if !chain && has(&lower) && !taken(&out, &lower) {
            out.push((lower, j));
            used[j] = true;
        }
    }
    let mut spines = Vec::new();
    let mut necks = Vec::new();
    for (j, jt) in bvh.joints.iter().enumerate() {
        if used[j] {
            continue;
        }
        let Some((side, k)) = classify(&jt.name) else {
            continue;
        };
        let target = match k {
            Kind::Sided(b) => match side {
                Some(s) => format!("{b}_{s}"),
                None => continue,
            },
            Kind::Plain(b) => b,
            Kind::Spine => {
                spines.push(j);
                continue;
            }
            Kind::Neck => {
                necks.push(j);
                continue;
            }
        };
        if has(&target) && !taken(&out, &target) {
            out.push((target, j));
            used[j] = true;
        }
    }
    if let Some(&n) = necks.first()
        && has("neck")
        && !taken(&out, "neck")
    {
        out.push(("neck".into(), n));
    }
    // Spine segments that are ancestors of the neck (all of them without one).
    let chain: Vec<usize> = match necks.first() {
        Some(&n) => spines
            .iter()
            .copied()
            .filter(|&s| bvh.is_ancestor(s, n))
            .collect(),
        None => spines.clone(),
    };
    if let Some(&first) = chain.first()
        && has("spine")
        && !taken(&out, "spine")
    {
        out.push(("spine".into(), first));
    }
    if chain.len() >= 2
        && let Some(&last) = chain.last()
        && has("chest")
        && !taken(&out, "chest")
    {
        out.push(("chest".into(), last));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: &str) -> Option<String> {
        classify(n).map(|(s, k)| match (s, k) {
            (Some(s), Kind::Sided(b)) => format!("{b}_{s}"),
            (_, Kind::Plain(b)) => b,
            (_, Kind::Spine) => "SPINE".into(),
            (_, Kind::Neck) => "NECK".into(),
            (None, Kind::Sided(b)) => format!("{b}_?"),
        })
    }

    #[test]
    fn conventions_classify() {
        for (n, want) in [
            ("Hips", "hip"),
            ("LeftUpLeg", "thigh_l"),
            ("RightLeg", "knee_r"),
            ("LeftFoot", "ankle_l"),
            ("LeftToeBase", "ball_l"),
            ("LeftToe_End", "toe_l"),
            ("Spine1", "SPINE"),
            ("Neck1", "NECK"),
            ("LeftShoulder", "clavicle_l"),
            ("LeftArm", "shoulder_l"),
            ("LeftForeArm", "elbow_l"),
            ("LeftHand", "wrist_l"),
            ("mixamorig:RightHandIndex2", "index_2_r"),
            ("mixamorig:LeftHandThumb1", "thumb_1_l"),
            ("Bip01 L Thigh", "thigh_l"),
            ("Bip01 R Calf", "knee_r"),
            ("Bip01 L UpperArm", "shoulder_l"),
            ("Bip01 L Finger0", "thumb_1_l"),
            ("Bip01 L Finger12", "index_3_l"),
            ("Bip01 Pelvis", "hip"),
            ("lfemur", "thigh_l"),
            ("rhumerus", "shoulder_r"),
            ("lowerback", "SPINE"),
            ("upperarm_l", "shoulder_l"),
            ("thigh_r", "thigh_r"),
            ("LThumb", "thumb_1_l"),
        ] {
            assert_eq!(t(n).as_deref(), Some(want), "{n}");
        }
        assert_eq!(t("LHipJoint"), None);
        assert_eq!(t("Bip01"), None);
    }
}
