//! OpenRaster（.ora）分层存档。
//!
//! 格式：zip 容器（首个条目 `mimetype` 必须为 stored 的 `image/openraster`）
//! + `stack.xml`（图层栈，首个 layer 元素为最顶层）+ `data/layerN.png`
//! + 每层一张画布尺寸透明 PNG + 可选 `mergedimage.png` / `Thumbnails/thumbnail.png`。
//!
//! 读写均为渲染无关的纯数据路径：图层 → PNG 走 1:1 瓦片拷贝，
//! 不需要 Renderer；合成预览由调用方（Engine）生成后传入。

use std::io::{Read, Write};

use crate::document::Document;
use crate::geometry::Rect;
use crate::layer::{BlendMode, Layer};
use crate::tile::{TileGrid, TileId, TILE};

/// BlendMode::ALL 顺序对应的 composite-op 名（W3C/SVG 命名，ORA 标准）。
const COMPOSITE_OPS: [&str; 12] = [
    "svg:src-over",
    "svg:multiply",
    "svg:screen",
    "svg:overlay",
    "svg:darken",
    "svg:lighten",
    "svg:color-dodge",
    "svg:color-burn",
    "svg:hard-light",
    "svg:soft-light",
    "svg:difference",
    "svg:exclusion",
];

fn op_to_mode(op: &str) -> BlendMode {
    COMPOSITE_OPS
        .iter()
        .position(|o| *o == op)
        .map(|i| BlendMode::ALL[i])
        .unwrap_or(BlendMode::Normal)
}

/// 载入的 ORA 工程数据。
pub struct OraLayer {
    pub name: String,
    /// 预乘 RGBA8 + 尺寸（data/layerN.png 解码结果）
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// 画布内偏移
    pub x: i32,
    pub y: i32,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: BlendMode,
}

pub struct OraDoc {
    pub w: u32,
    pub h: u32,
    /// 与 stack.xml 相同顺序：首个元素为最顶层
    pub layers: Vec<OraLayer>,
}

/// 图层 1:1 拷贝到 RGBA 预乘缓冲（bounds 为画布像素矩形）。
fn layer_to_rgba(layer: &Layer, bounds: Rect, canvas: (u32, u32)) -> Vec<u8> {
    let (cw, ch) = canvas;
    let mut buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
    for (tid, tile) in layer.tiles.iter_entries() {
        let (ox, oy) = (tid.origin().0, tid.origin().1);
        for ty in 0..TILE as i64 {
            let gy = oy + ty;
            if gy < bounds.y as i64 || gy >= bounds.y2() {
                continue;
            }
            for tx in 0..TILE as i64 {
                let gx = ox + tx;
                if gx < bounds.x as i64 || gx >= bounds.x2() {
                    continue;
                }
                // 画布坐标 → 缓冲局部坐标（bounds 原点）
                let lx = (gx - bounds.x as i64) as usize;
                let ly = (gy - bounds.y as i64) as usize;
                let s = ((ty * TILE as i64 + tx) * 4) as usize;
                let d = (ly * cw as usize + lx) * 4;
                buf[d..d + 4].copy_from_slice(&tile.pixels()[s..s + 4]);
            }
        }
    }
    buf
}

/// RGBA 预乘缓冲写入图层瓦片（原点对齐）。
fn rgba_into_grid(rgba: &[u8], w: u32, h: u32, grid: &mut TileGrid) {
    for ty in 0..h.div_ceil(TILE) {
        for tx in 0..w.div_ceil(TILE) {
            let id = TileId {
                x: tx as i32,
                y: ty as i32,
            };
            let mut any = false;
            let tile = grid.get_or_create_mut(id);
            let px = tile.pixels_mut();
            for row in 0..TILE {
                let gy = row + ty * TILE;
                if gy >= h {
                    break;
                }
                for col in 0..TILE {
                    let gx = col + tx * TILE;
                    if gx >= w {
                        break;
                    }
                    let s = ((gy * w + gx) * 4) as usize;
                    let d = ((row * TILE + col) * 4) as usize;
                    if rgba[s + 3] > 0 {
                        any = true;
                    }
                    px[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
                }
            }
            if !any {
                grid.remove(id); // 全透明块不占内存
            }
        }
    }
}

/// 编码 .ora。`merged`/`thumbnail` 为可选的合成 PNG（由 Engine 生成）。
pub fn encode_ora(
    doc: &Document,
    merged: Option<&[u8]>,
    thumbnail: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    // 画布 = 全部图层内容包围盒（无内容时 1×1）
    let mut bounds: Option<Rect> = None;
    for l in doc.layers().iter() {
        if let Some(b) = l.tiles.content_bounds_precise() {
            bounds = Some(match bounds {
                Some(a) => a.union(&b),
                None => b,
            });
        }
    }
    let bounds = bounds.unwrap_or(Rect::new(0, 0, 1, 1));
    let (cw, ch) = (bounds.w.max(1), bounds.h.max(1));

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // mimetype 必须是首个条目且 stored
    zip.start_file("mimetype", stored)
        .map_err(|e| format!("mimetype 写入失败: {e}"))?;
    zip.write_all(b"image/openraster")
        .map_err(|e| e.to_string())?;

    // 图层 PNG（data/layerN.png），N 按栈序自底向上编号
    let layers: Vec<_> = doc.layers().iter_with_id().collect();
    let mut pngs: Vec<(usize, Vec<u8>)> = Vec::new();
    for (i, (_, layer)) in layers.iter().enumerate() {
        let rgba = layer_to_rgba(layer, bounds, (cw, ch));
        let png = crate::io::encode_png(&rgba, cw, ch)?;
        zip.start_file(format!("data/layer{i}.png"), deflated)
            .map_err(|e| e.to_string())?;
        zip.write_all(&png).map_err(|e| e.to_string())?;
        pngs.push((i, png));
    }

    // stack.xml：首个 layer = 最顶层
    let mut xml = String::new();
    xml.push_str("<?xml version='1.0' encoding='UTF-8'?>\n");
    xml.push_str(&format!(
        "<image version=\"0.0.3\" w=\"{cw}\" h=\"{ch}\" xsrc=\"mergedimage.png\">\n<stack>\n"
    ));
    for (i, (_, layer)) in layers.iter().enumerate().rev() {
        let op = BlendMode::ALL
            .iter()
            .position(|m| *m == layer.blend_mode)
            .unwrap_or(0);
        let vis = if layer.visible { "visible" } else { "hidden" };
        let name = layer.name.replace(['&', '<', '>', '"', '\''], "_");
        xml.push_str(&format!(
            "  <layer name=\"{name}\" src=\"data/layer{i}.png\" x=\"{}\" y=\"{}\" opacity=\"{:.4}\" visibility=\"{vis}\" composite-op=\"{}\"/>\n",
            bounds.x,
            bounds.y,
            layer.opacity.clamp(0.0, 1.0),
            COMPOSITE_OPS[op],
        ));
    }
    xml.push_str("</stack>\n</image>\n");
    zip.start_file("stack.xml", deflated)
        .map_err(|e| e.to_string())?;
    zip.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;

    if let Some(m) = merged {
        zip.start_file("mergedimage.png", deflated)
            .map_err(|e| e.to_string())?;
        zip.write_all(m).map_err(|e| e.to_string())?;
    }
    if let Some(t) = thumbnail {
        zip.start_file("Thumbnails/thumbnail.png", deflated)
            .map_err(|e| e.to_string())?;
        zip.write_all(t).map_err(|e| e.to_string())?;
    }

    let cursor = zip.finish().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

/// 解码 .ora。
pub fn decode_ora(bytes: &[u8]) -> Result<OraDoc, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("不是有效的 zip/ORA: {e}"))?;
    let mut xml = String::new();
    zip.by_name("stack.xml")
        .map_err(|e| format!("缺少 stack.xml: {e}"))?
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    let parsed =
        roxmltree::Document::parse(&xml).map_err(|e| format!("stack.xml 解析失败: {e}"))?;

    let img = parsed.root_element();
    if img.tag_name().name() != "image" {
        return Err("stack.xml 根元素不是 image".into());
    }
    let w: u32 = img
        .attribute("w")
        .and_then(|v| v.parse().ok())
        .ok_or("缺少画布宽度")?;
    let h: u32 = img
        .attribute("h")
        .and_then(|v| v.parse().ok())
        .ok_or("缺少画布高度")?;

    let mut layers = Vec::new();
    for node in parsed
        .descendants()
        .filter(|n| n.tag_name().name() == "layer")
    {
        let src = node.attribute("src").ok_or("layer 缺少 src")?;
        let mut png = Vec::new();
        zip.by_name(src)
            .map_err(|e| format!("缺少 {src}: {e}"))?
            .read_to_end(&mut png)
            .map_err(|e| e.to_string())?;
        let (rgba, lw, lh) = crate::io::decode_png(&png)?;
        layers.push(OraLayer {
            name: node.attribute("name").unwrap_or("图层").to_string(),
            rgba,
            w: lw,
            h: lh,
            x: node
                .attribute("x")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            y: node
                .attribute("y")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            opacity: node
                .attribute("opacity")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            visible: node.attribute("visibility").is_none_or(|v| v == "visible"),
            blend_mode: node
                .attribute("composite-op")
                .map_or(BlendMode::Normal, op_to_mode),
        });
    }
    if layers.is_empty() {
        return Err("ORA 中没有图层".into());
    }
    Ok(OraDoc { w, h, layers })
}

/// 从 OraDoc 构建图层栈（自底向上插入）。
pub fn layers_from_ora(ora: &OraDoc) -> Vec<Layer> {
    let mut out = Vec::new();
    for l in ora.layers.iter().rev() {
        let mut layer = Layer::new(l.name.clone());
        layer.opacity = l.opacity.clamp(0.0, 1.0);
        layer.visible = l.visible;
        layer.blend_mode = l.blend_mode;
        // PNG 是画布尺寸、偏移由 x/y 给出：解到临时网格再整体平移放置
        let mut grid = TileGrid::new();
        rgba_into_grid(&l.rgba, l.w, l.h, &mut grid);
        if l.x != 0 || l.y != 0 {
            let shifted = shift_grid(&grid, l.x, l.y);
            layer.tiles = shifted;
        } else {
            layer.tiles = grid;
        }
        out.push(layer);
    }
    out
}

/// 网格整体平移（tile 重组）。
fn shift_grid(grid: &TileGrid, dx: i32, dy: i32) -> TileGrid {
    let mut out = TileGrid::new();
    for (tid, tile) in grid.iter_entries() {
        // 求瓦片内内容 bbox 以整块平移
        let mut min = (i64::MAX, i64::MAX);
        let mut max = (i64::MIN, i64::MIN);
        let px = tile.pixels();
        for y in 0..TILE as usize {
            let mut row_hit = false;
            for x in 0..TILE as usize {
                if px[(y * TILE as usize + x) * 4 + 3] > 0 {
                    row_hit = true;
                    min.0 = min.0.min(x as i64);
                    max.0 = max.0.max(x as i64);
                }
            }
            if row_hit {
                min.1 = min.1.min(y as i64);
                max.1 = max.1.max(y as i64);
            }
        }
        if max.0 < min.0 {
            continue;
        }
        let (ox, oy) = (
            tid.origin().0 + min.0 + dx as i64,
            tid.origin().1 + min.1 + dy as i64,
        );
        for y in min.1..=max.1 {
            for x in min.0..=max.0 {
                let src = ((y as usize * TILE as usize) + x as usize) * 4;
                let dst_id = TileId::at(ox + x - min.0, oy + y - min.1);
                let dst_tile = out.get_or_create_mut(dst_id);
                let (dox, doy) = dst_id.origin();
                let lx = (ox + x - min.0 - dox) as usize;
                let ly = (oy + y - min.1 - doy) as usize;
                let d = ((ly * TILE as usize) + lx) * 4;
                dst_tile.pixels_mut()[d..d + 4].copy_from_slice(&px[src..src + 4]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::tile::TileId;

    /// 在图层 (x0,y0) 起 size×size 涂纯色块（不经渲染器）。
    fn fill(layer: &mut Layer, x0: i32, y0: i32, size: i32, c: [u8; 4]) {
        let mut ids = Vec::new();
        for ty in (y0 >> 8)..=((y0 + size - 1) >> 8) {
            for tx in (x0 >> 8)..=((x0 + size - 1) >> 8) {
                ids.push(TileId { x: tx, y: ty });
            }
        }
        for id in ids {
            let (ox, oy) = id.origin();
            let tile = layer.tiles.get_or_create_mut(id);
            let px = tile.pixels_mut();
            for y in y0..y0 + size {
                for x in x0..x0 + size {
                    let lx = (x - ox as i32) as usize;
                    let ly = (y - oy as i32) as usize;
                    if lx < 256 && ly < 256 {
                        let i = (ly * 256 + lx) * 4;
                        px[i..i + 4].copy_from_slice(&c);
                    }
                }
            }
        }
    }

    fn doc_with_stroke() -> Document {
        let mut doc = Document::new(usize::MAX);
        doc.set_background(Color::WHITE);
        doc.set_show_grid(false);
        let l1 = doc.active_layer();
        doc.layers_mut().get_mut(l1).opacity = 1.0;
        fill(doc.layers_mut().get_mut(l1), 90, 70, 40, [0, 0, 0, 255]);
        let l2 = doc.layers_mut().insert(None);
        {
            let layer = doc.layers_mut().get_mut(l2);
            fill(layer, 130, 90, 24, [200, 40, 40, 255]);
            layer.opacity = 0.7;
            layer.blend_mode = BlendMode::Multiply;
            layer.name = "红层".into();
        }
        doc
    }

    #[test]
    fn ora_roundtrip() {
        let doc = doc_with_stroke();
        let bytes = encode_ora(&doc, None, None).unwrap();
        let ora = decode_ora(&bytes).unwrap();
        assert_eq!(ora.layers.len(), 2);
        // 首个 = 顶层 = 红层
        let top = &ora.layers[0];
        assert_eq!(top.name, "红层");
        assert!((top.opacity - 0.7).abs() < 1e-3);
        assert_eq!(top.blend_mode, BlendMode::Multiply);
        assert!(top.visible);

        // 像素往返：顶层中心应仍有红（直行→PNG→预乘，不透明像素无损）
        let layers = layers_from_ora(&ora);
        assert_eq!(layers.len(), 2);
        let top_l = &layers[1]; // 顶层：红方块 (130..154, 90..114)（layers 自底向上）
        let tid = TileId::at(140, 100);
        let t = top_l.tiles.get(tid).expect("顶层瓦片应存在");
        let i = ((100 & 255) as usize * TILE as usize + (140 & 255) as usize) * 4;
        let p = &t.pixels()[i..i + 4];
        // 扫瓦片找实际内容位置
        let mut hits = vec![];
        for y in 0..TILE as usize {
            for x in 0..TILE as usize {
                if t.pixels()[(y * TILE as usize + x) * 4 + 3] > 0 && hits.len() < 4 {
                    hits.push((x, y));
                }
            }
        }
        println!(
            "top tile 内容首像素: {:?} (期望 x∈130..154 y∈90..114)",
            hits
        );
        assert_eq!(p[3], 255);
        assert_eq!(p[0], 200);
        // 底层黑方块也在
        let bottom_l = &layers[0];
        let t2 = bottom_l.tiles.get(TileId::at(100, 80)).expect("底层瓦片");
        let i2 = ((80 & 255) as usize * TILE as usize + (100 & 255) as usize) * 4;
        assert_eq!(t2.pixels()[i2 + 3], 255);
    }

    #[test]
    fn layer_offsets_applied() {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"image/openraster").unwrap();
        // 4×4 全白 PNG 放在 (50, 60)
        let rgba = vec![255u8; 4 * 4 * 4];
        let png = crate::io::encode_png(&rgba, 4, 4).unwrap();
        zip.start_file("data/layer0.png", deflated).unwrap();
        zip.write_all(&png).unwrap();
        zip.start_file("stack.xml", deflated).unwrap();
        zip.write_all(
            b"<?xml version='1.0' encoding='UTF-8'?>
<image version=\"0.0.3\" w=\"128\" h=\"128\">
<stack><layer name=\"off\" src=\"data/layer0.png\" x=\"50\" y=\"60\" opacity=\"1.0\" visibility=\"visible\" composite-op=\"svg:src-over\"/></stack>
</image>",
        )
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();

        let ora = decode_ora(&bytes).unwrap();
        let layers = layers_from_ora(&ora);
        let g = &layers[0].tiles;
        // (50,60) 有内容，(49,60) 没有
        assert!(g.get(TileId::at(50, 60)).is_some());
        let inside = g.get(TileId::at(52, 62)).unwrap();
        let i = ((62 & 255) as usize * TILE as usize + (52 & 255) as usize) * 4;
        assert_eq!(inside.pixels()[i + 3], 255);
        assert!(g.get(TileId::at(49, 59)).is_none_or(|t| {
            let i = ((59 & 255) as usize * TILE as usize + (49 & 255) as usize) * 4;
            t.pixels()[i + 3] == 0
        }));
    }
}
