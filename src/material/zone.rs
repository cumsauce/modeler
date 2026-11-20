//! `zone`: a 0..1 mask of a named body region of the human template
//! (`lips`, `nails`, `cheeks`, ...), read from the template's uv layout,
//! so a material paints by region with no uv knowledge (lipstick, blush,
//! nail colour, dirt on the hands).

use serde::Deserialize;

use super::node::{Buf, Cx, Eval, Outs, pf};

/// A zone of the human template (`crate::human::landmarks::ZONES`):
/// 1 inside, 0 outside, smoothed by `soft` meters. It reads the sample's
/// uv as the template's (the human body's built-in skin is a card-domain
/// material over the template uvs), so it means something on a human
/// body part; in the object domain it is 0 everywhere (neutral).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    /// lips, face, scalp, brows, nails, areola, hands, feet, ears, nose,
    /// cheeks, knees, elbows, knuckles, tzone.
    pub zone: String,
    /// Extra softness of the edge, meters (default 0).
    #[serde(default)]
    pub soft: Option<f32>,
}

impl Eval for Zone {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let n = cx.n();
        if !crate::human::landmarks::ZONES.contains(&self.zone.as_str()) {
            return Err(format!(
                "zone: `{}` is not a zone (one of {})",
                self.zone,
                crate::human::landmarks::ZONES.join(", ")
            ));
        }
        if cx.s.valid.is_some() || !crate::human::asset::available() {
            return Ok(vec![("", Buf::F(vec![0.0; n]))]);
        }
        let res = 1024usize;
        let img = crate::human::landmarks::zone_image(&self.zone, res, self.soft.unwrap_or(0.0))?;
        let period = if cx.s.period > 0.0 { cx.s.period } else { 1.0 };
        let uv = cx.s.uv.clone();
        Ok(vec![(
            "",
            pf(n, |i| {
                let p = match &*uv {
                    Buf::V2(v) => v[i],
                    _ => [0.0, 0.0],
                };
                crate::human::landmarks::sample(&img, res, [p[0] / period, p[1] / period])
            }),
        )])
    }
}
