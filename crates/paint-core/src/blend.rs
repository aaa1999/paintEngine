use crate::layer::BlendMode;

/// 分离混合模式：按通道的混合函数 B(Cb, Cs)，输入输出均为
/// 去 alpha 的直行色 0..1。公式取自 W3C Compositing and Blending Level 1。
pub fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::SoftLight => soft_light(cb, cs),
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
    }
}

fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        2.0 * cb * cs
    } else {
        1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
    }
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

/// 源（图层像素，预乘）以 `opacity × blend_mode` 合成到 `dst`（预乘）上。
/// Porter-Duff source-over + blend 的一般式，dst 可半透明（merge 场景）。
pub fn composite_pixel(dst: &mut [u8], src: &[u8], opacity: f32, mode: BlendMode) {
    let as_raw = src[3] as f32 / 255.0;
    if as_raw <= 0.0 {
        return;
    }
    let op = opacity.clamp(0.0, 1.0);
    let as_eff = as_raw * op;
    if as_eff <= 0.0 {
        return;
    }
    let ab = dst[3] as f32 / 255.0;

    let ao = as_eff + ab * (1.0 - as_eff);
    if ao <= 0.0 {
        dst[0] = 0;
        dst[1] = 0;
        dst[2] = 0;
        dst[3] = 0;
        return;
    }

    // 每通道：Co = αs·[(1−αb)·Cs + αb·B(Cb,Cs)] + (1−αs)·Cb   （均为预乘域）
    for i in 0..3 {
        let cs = src[i] as f32 / 255.0 / as_raw; // 直行源色
        let cb = if ab > 0.0 {
            dst[i] as f32 / 255.0 / ab
        } else {
            0.0
        };
        let co =
            as_eff * ((1.0 - ab) * cs + ab * blend_channel(mode, cb, cs)) + (1.0 - as_eff) * cb;
        dst[i] = (co.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    dst[3] = (ao * 255.0 + 0.5) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
        [r, g, b, a]
    }

    #[test]
    fn normal_matches_source_over() {
        let mut d = px(255, 255, 255, 255);
        composite_pixel(&mut d, &px(0, 0, 0, 255), 1.0, BlendMode::Normal);
        assert_eq!(d, px(0, 0, 0, 255));
        let mut d = px(255, 255, 255, 255);
        composite_pixel(&mut d, &px(0, 0, 0, 128), 1.0, BlendMode::Normal);
        assert_eq!(d, px(127, 127, 127, 255));
    }

    #[test]
    fn multiply_darkens_white_unchanged() {
        // 红 (255,0,0) multiply 白 (255,255,255) → 红
        let mut d = px(255, 255, 255, 255);
        composite_pixel(&mut d, &px(255, 0, 0, 255), 1.0, BlendMode::Multiply);
        assert_eq!(d, px(255, 0, 0, 255));
        // 红 multiply 黑 → 黑
        let mut d = px(0, 0, 0, 255);
        composite_pixel(&mut d, &px(255, 0, 0, 255), 1.0, BlendMode::Multiply);
        assert_eq!(d, px(0, 0, 0, 255));
    }

    #[test]
    fn screen_ignores_black_keeps_white() {
        let mut d = px(20, 40, 60, 255);
        composite_pixel(&mut d, &px(0, 0, 0, 255), 1.0, BlendMode::Screen);
        assert_eq!(d, px(20, 40, 60, 255), "黑源 screen 不改变底色");
        let mut d = px(20, 40, 60, 255);
        composite_pixel(&mut d, &px(255, 255, 255, 255), 1.0, BlendMode::Screen);
        assert_eq!(d, px(255, 255, 255, 255));
    }

    #[test]
    fn difference_and_exclusion() {
        let mut d = px(200, 100, 50, 255);
        composite_pixel(&mut d, &px(50, 150, 50, 255), 1.0, BlendMode::Difference);
        assert_eq!(d, px(150, 50, 0, 255));
        let mut d = px(200, 100, 50, 255);
        composite_pixel(&mut d, &px(50, 150, 50, 255), 1.0, BlendMode::Exclusion);
        // Cb+Cs−2CbCs（与实现一致的 +0.5 舍入）
        let e = |cb: f32, cs: f32| ((cb + cs - 2.0 * cb * cs) * 255.0 + 0.5) as u8;
        assert_eq!(
            d,
            px(
                e(200.0 / 255.0, 50.0 / 255.0),
                e(100.0 / 255.0, 150.0 / 255.0),
                e(50.0 / 255.0, 50.0 / 255.0),
                255
            )
        );
    }

    #[test]
    fn blend_onto_transparent_keeps_premul() {
        // 半透明黑底（预乘域 alpha 128）：其直行色 Cb=0。
        // 不透明红 Multiply：Co = 1·[(1−αb)·1 + αb·0] ≈ 0.498 → 127
        let mut d = px(0, 0, 0, 128);
        composite_pixel(&mut d, &px(255, 0, 0, 255), 1.0, BlendMode::Multiply);
        assert_eq!(d, px(127, 0, 0, 255));
    }

    #[test]
    fn opacity_zero_noop() {
        let mut d = px(1, 2, 3, 4);
        composite_pixel(&mut d, &px(255, 0, 0, 255), 0.0, BlendMode::Normal);
        assert_eq!(d, px(1, 2, 3, 4));
    }

    #[test]
    fn all_modes_finite() {
        // 边界值遍历，保证无 NaN/越界
        for mode in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
        ] {
            for cb in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                for cs in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                    let v = blend_channel(mode, cb, cs);
                    assert!(
                        (0.0..=1.0001).contains(&v) && v.is_finite(),
                        "{mode:?} {cb} {cs} -> {v}"
                    );
                }
            }
        }
    }
}
