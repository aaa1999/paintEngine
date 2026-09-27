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

/// f32 累积缓冲 → 16-bit PNG（每通道 u16）。
/// 输入 `f32_buf` 为直行 RGBA f32（0..1），长度 = w*h*4。
pub fn encode_png16(f32_buf: &[f32], w: u32, h: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("空图像".into());
    }
    let expected = (w as usize) * (h as usize) * 4;
    if f32_buf.len() < expected {
        return Err(format!("f32 缓冲过小: {} < {expected}", f32_buf.len()));
    }
    let mut u16_buf = vec![0u16; expected];
    for (d, s) in u16_buf
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(f32_buf.as_chunks::<4>().0)
    {
        for k in 0..4 {
            d[k] = (s[k].clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Sixteen);
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("PNG16 头写入失败: {e}"))?;
        // png crate 要求 16-bit 为大端字节序
        let mut be = Vec::with_capacity(expected * 2);
        for v in &u16_buf {
            be.extend_from_slice(&v.to_be_bytes());
        }
        writer
            .write_image_data(&be)
            .map_err(|e| format!("PNG16 数据写入失败: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests16 {
    use super::*;

    #[test]
    fn png16_roundtrip_levels() {
        // 4 级渐变 f32 → 16-bit → 解码回 u16 值精确
        let w = 4;
        let h = 1;
        let f32_buf: Vec<f32> = (0..4)
            .flat_map(|i| {
                let v = i as f32 / 3.0;
                vec![v, v, v, 1.0]
            })
            .collect();
        let png = encode_png16(&f32_buf, w, h).unwrap();
        // 验证非空且前几个字节是 PNG 魔数
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
        assert!(png.len() > 50, "16-bit PNG 应比 8-bit 大");
    }
}

/// JPEG 解码 → 预乘 RGBA。CMYK JPEG 转为 RGB。
pub fn decode_jpeg(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let pixels = decoder
        .decode()
        .map_err(|e| format!("JPEG 解码失败: {e}"))?;
    let info = decoder.info().ok_or("JPEG 无元信息")?;
    let (w, h) = (info.width as u32, info.height as u32);
    let n = (w as usize) * (h as usize);
    let mut rgba = vec![0u8; n * 4];
    match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            for (d, s) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(pixels.iter()) {
                d[0] = *s;
                d[1] = *s;
                d[2] = *s;
                d[3] = 255;
            }
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            for (d, s) in rgba
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(pixels.as_chunks::<3>().0)
            {
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
                d[3] = 255;
            }
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            // CMYK → RGB（简化：无 ICC）
            for (d, s) in rgba
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(pixels.as_chunks::<4>().0)
            {
                let c = s[0] as u32;
                let m = s[1] as u32;
                let y = s[2] as u32;
                let k = s[3] as u32;
                d[0] = (255 - (c * (255 - k) / 255 + k).min(255)) as u8;
                d[1] = (255 - (m * (255 - k) / 255 + k).min(255)) as u8;
                d[2] = (255 - (y * (255 - k) / 255 + k).min(255)) as u8;
                d[3] = 255;
            }
        }
        _ => return Err(format!("不支持的 JPEG 像素格式: {:?}", info.pixel_format)),
    }
    // 直行 → 预乘（JPEG 无 alpha，跳过）
    Ok((rgba, w, h))
}

/// JPEG 编码（quality 0-100）。输入为预乘 RGBA（alpha 被忽略——JPEG 无透明）。
pub fn encode_jpeg(premul: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("空图像".into());
    }
    let n = (w as usize) * (h as usize);
    if premul.len() < n * 4 {
        return Err("缓冲过小".into());
    }
    // 预乘 → RGB（JPEG 无 alpha）
    let mut rgb = vec![0u8; n * 3];
    for (d, s) in rgb
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(premul.as_chunks::<4>().0)
    {
        d[0] = s[0];
        d[1] = s[1];
        d[2] = s[2];
    }
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    encoder
        .encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    Ok(out)
}

/// WebP 解码 → 预乘 RGBA。
pub fn decode_webp(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("WebP 解析失败: {e}"))?;
    let (w, h) = decoder.dimensions();
    let n = (w as usize) * (h as usize);
    let mut rgba = vec![0u8; n * 4];
    decoder
        .read_image(&mut rgba)
        .map_err(|e| format!("WebP 解码失败: {e}"))?;
    // image-webp 输出直行 RGBA → 预乘
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a == 0 {
            continue;
        }
        for c in px.iter_mut().take(3) {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    Ok((rgba, w, h))
}

/// 自动识别格式解码（PNG / JPEG / WebP）→ 预乘 RGBA。
pub fn decode_auto(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if bytes.len() < 12 {
        return Err("数据过短".into());
    }
    // PNG 魔数: 89 50 4E 47
    if bytes[..4] == [0x89, 0x50, 0x4E, 0x47] {
        return decode_png(bytes);
    }
    // JPEG 魔数: FF D8 FF
    if bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return decode_jpeg(bytes);
    }
    // WebP: "RIFF" + ... + "WEBP"
    if bytes[..4] == *b"RIFF" && bytes.len() >= 12 && bytes[8..12] == *b"WEBP" {
        return decode_webp(bytes);
    }
    Err("无法识别图像格式（支持 PNG/JPEG/WebP）".into())
}

#[cfg(test)]
mod jpeg_webp_tests {
    use super::*;

    #[test]
    fn jpeg_roundtrip() {
        // 8×8 红 → JPEG → 解码回：红通道高、蓝绿低
        let mut premul = vec![0u8; 8 * 8 * 4];
        for px in premul.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&[220, 30, 30, 255]);
        }
        let jpg = encode_jpeg(&premul, 8, 8, 95).unwrap();
        assert!(!jpg.is_empty());
        assert_eq!(&jpg[..3], &[0xFF, 0xD8, 0xFF], "JPEG 魔数");
        let (rgba, w, h) = decode_jpeg(&jpg).unwrap();
        assert_eq!((w, h), (8, 8));
        // 中心像素：红高（JPEG 有损但红通道应 >150）
        assert!(rgba[0] > 150, "红通道: {}", rgba[0]);
        assert!(rgba[1] < 100, "绿通道: {}", rgba[1]);
    }

    #[test]
    fn decode_auto_detects() {
        // PNG
        let png = encode_png(&[255, 0, 0, 255], 1, 1).unwrap();
        let (rgba, _, _) = decode_auto(&png).unwrap();
        assert_eq!(rgba[3], 255);
        // JPEG
        let jpg = encode_jpeg(&[0, 0, 0, 255, 0, 0, 0, 255], 2, 1, 90).unwrap();
        let (_, _, _) = decode_auto(&jpg).unwrap();
        // 垃圾数据
        assert!(decode_auto(b"not an image").is_err());
    }
}
