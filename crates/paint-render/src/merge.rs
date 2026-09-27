use paint_core::blend::composite_pixel;
use paint_core::history::StrokeRecorder;
use paint_core::layer::Layer;

/// 把 `src` 图层按其 opacity/blend_mode 合入 `dst`（瓦片对瓦片，1:1
/// 像素对应，无缩放）。merge_down / flatten / 导入用。
/// `src` 不可见时直接跳过（与主流软件一致：隐藏图层合并不贡献像素）。
pub fn merge_layers(dst: &mut Layer, src: &Layer, recorder: &mut StrokeRecorder) {
    if !src.visible {
        return;
    }
    let ids: Vec<_> = src.tiles.ids().collect();
    for tid in ids {
        let Some(s_tile) = src.tiles.get(tid) else {
            continue;
        };
        recorder.capture(&dst.tiles, tid);
        let d_tile = dst.tiles.get_or_create_mut(tid);
        let sp = s_tile.pixels();
        let dp = d_tile.pixels_mut();
        for (d, s) in dp
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(sp.as_chunks::<4>().0)
        {
            if s[3] == 0 {
                continue;
            }
            composite_pixel(d, s, src.opacity, src.blend_mode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paint_core::color::Color;
    use paint_core::layer::{BlendMode, LayerStack};
    use paint_core::stroke::{Dab, DabMode};
    use paint_core::tile::TILE;

    fn paint_layer(layers: &mut LayerStack, id: paint_core::layer::LayerId) {
        let dabs = vec![Dab {
            x: 64.0,
            y: 64.0,
            radius: 10.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
        }];
        let l = layers.get_mut(id);
        super::super::stamp::stamp_dabs(l, &dabs, &mut StrokeRecorder::new(id));
    }

    fn pixel_at(layer: &Layer, x: usize, y: usize) -> [u8; 4] {
        let tid = paint_core::tile::TileId::at(x as i64, y as i64);
        let t = layer.tiles.get(tid).expect("瓦片应存在");
        let lx = x & 255;
        let ly = y & 255;
        t.pixels()[(ly * TILE as usize + lx) * 4..][..4]
            .try_into()
            .unwrap()
    }

    #[test]
    fn merge_combines_content() {
        let mut s = LayerStack::new();
        let bottom = s.insert(None);
        let top = s.insert(None);
        paint_layer(&mut s, bottom);
        paint_layer(&mut s, top);
        let (bottom_layer, top_layer) = {
            let b = s.get(bottom).clone();
            let t = s.get(top).clone();
            (b, t)
        };
        let mut dst = bottom_layer;
        merge_layers(&mut dst, &top_layer, &mut StrokeRecorder::new(bottom));
        assert_eq!(pixel_at(&dst, 64, 64)[3], 255, "两层黑叠加仍不透明");
        // top 偏移不覆盖区域也应保留 bottom 内容
        assert!(pixel_at(&dst, 60, 64)[3] > 0);
    }

    #[test]
    fn merge_respects_blend_and_opacity() {
        let mut s = LayerStack::new();
        let bottom = s.insert(None);
        let top = s.insert(None);
        paint_layer(&mut s, bottom);
        paint_layer(&mut s, top);
        s.get_mut(top).opacity = 0.5;
        s.get_mut(top).blend_mode = BlendMode::Multiply;
        let (bottom_layer, top_layer) = {
            let b = s.get(bottom).clone();
            let t = s.get(top).clone();
            (b, t)
        };
        let mut dst = bottom_layer;
        merge_layers(&mut dst, &top_layer, &mut StrokeRecorder::new(bottom));
        let c = pixel_at(&dst, 64, 64);
        // 黑 50% multiply 黑 → 仍黑；但边缘半透明区域 alpha 不变
        assert_eq!(c[3], 255);
    }

    #[test]
    fn hidden_src_skipped() {
        let mut s = LayerStack::new();
        let bottom = s.insert(None);
        let top = s.insert(None);
        paint_layer(&mut s, top);
        s.get_mut(top).visible = false;
        let (bottom_layer, top_layer) = {
            let b = s.get(bottom).clone();
            let t = s.get(top).clone();
            (b, t)
        };
        let mut dst = bottom_layer;
        merge_layers(&mut dst, &top_layer, &mut StrokeRecorder::new(bottom));
        assert!(dst.tiles.is_empty(), "隐藏图层不贡献像素");
    }
}
