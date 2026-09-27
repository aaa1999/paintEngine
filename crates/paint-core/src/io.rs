//! PNG 编解码。内部像素为 RGBA8 预乘；PNG 标准为直行 alpha，
//! 边界处做预乘 ↔ 直行换算。

/// 编码预乘 RGBA8 帧为 PNG 字节。
pub fn encode_png(premul: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("空图像".into());
    }
    let expected = w as usize * h as usize * 4;
    if premul.len() < expected {
        return Err(format!("缓冲过小: {} < {expected}", premul.len()));
    }
    let mut straight = vec![0u8; expected];
    for (d, s) in straight
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(premul.as_chunks::<4>().0)
    {
        let a = s[3] as u32;
        if a == 0 {
            continue;
        }
        *d = [
            ((s[0] as u32 * 255 + a / 2) / a).min(255) as u8,
            ((s[1] as u32 * 255 + a / 2) / a).min(255) as u8,
            ((s[2] as u32 * 255 + a / 2) / a).min(255) as u8,
            s[3],
        ];
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("PNG 头写入失败: {e}"))?;
        writer
            .write_image_data(&straight)
            .map_err(|e| format!("PNG 数据写入失败: {e}"))?;
    }
    Ok(out)
}

/// 解码 PNG 为预乘 RGBA8，返回 (像素, 宽, 高)。
pub fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let dec = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = dec.read_info().map_err(|e| format!("PNG 解析失败: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("PNG 解码失败: {e}"))?;
    buf.truncate(info.buffer_size());

    // 统一到 RGBA8
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let n = info.width as usize * info.height as usize;
            let mut out = Vec::with_capacity(n * 4);
            for px in buf.as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let n = info.width as usize * info.height as usize;
            let mut out = Vec::with_capacity(n * 4);
            for px in &buf[..n] {
                out.extend_from_slice(&[*px, *px, *px, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let n = info.width as usize * info.height as usize;
            let mut out = Vec::with_capacity(n * 4);
            for px in buf.as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        other => return Err(format!("不支持的 PNG 颜色类型: {other:?}")),
    };

    // 直行 → 预乘
    let mut premul = rgba;
    for px in premul.as_chunks_mut::<4>().0.iter_mut() {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
    }
    Ok((premul, info.width, info.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_premul(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        // 输入为直行，先转预乘
        let af = a as u32;
        vec![
            ((r as u32 * af + 127) / 255) as u8,
            ((g as u32 * af + 127) / 255) as u8,
            ((b as u32 * af + 127) / 255) as u8,
            a,
        ]
    }

    #[test]
    fn roundtrip_opaque() {
        let mut frame = vec![0u8; 4 * 4 * 4];
        for px in frame.as_chunks_mut::<4>().0.iter_mut() {
            *px = [255, 0, 0, 255];
        }
        let png = encode_png(&frame, 4, 4).unwrap();
        let (dec, w, h) = decode_png(&png).unwrap();
        assert_eq!((w, h), (4, 4));
        assert!(dec
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [255, 0, 0, 255]));
    }

    #[test]
    fn roundtrip_semi_transparent() {
        let mut frame = vec![0u8; 4 * 4];
        frame[0..4].copy_from_slice(&solid_premul(200, 100, 50, 128));
        let png = encode_png(&frame, 1, 1).unwrap();
        let (dec, w, h) = decode_png(&png).unwrap();
        assert_eq!((w, h), (1, 1));
        let p = &dec[0..4];
        assert_eq!(p[3], 128);
        // 预乘域往返（直行 200·128/255 ≈ 100，量化误差 ±1）
        assert!((p[0] as i32 - 100).abs() <= 1, "{}", p[0]);
        assert!((p[1] as i32 - 50).abs() <= 1);
        assert!((p[2] as i32 - 25).abs() <= 1);
    }

    #[test]
    fn decode_rgb_png() {
        // 用 png 编码器造一张 RGB 图（走 decode 分支）
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 2);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[10, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30])
                .unwrap();
        }
        let (dec, w, h) = decode_png(&out).unwrap();
        assert_eq!((w, h), (2, 2));
        assert!(dec
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [10, 20, 30, 255]));
    }
}
