//! 表格图片单元格的缩略图与像素解码。
//!
//! 单元格只按显示尺寸需要纹理：原图按原始分辨率上传会占大量显存，且解码后超过阈值的图
//! 由渲染层后台线程上传（上传完成前格子是空的）。缩略图把长边压到 `THUMB_MAX_EDGE` 以内，
//! 单元格用同一份像素，解码与缩放都留在后台线程。
//!
//! 解码口径与渲染层一致：EXIF 方向在解码后立即应用——渲染层解码原图时同样应用，
//! 否则单元格与灯箱的朝向不同。

use std::io::Cursor;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader};

/// 单元格缩略图长边上限（像素）。
///
/// 取 720 使解码后不超过 2 MiB（`720 * 720 * 4`）：渲染层对该阈值以内的纹理同帧内同步
/// 上传，缩略图滚出视口再滚回时不会出现空白帧。单元格最大显示尺寸（缩放 2×、200% 缩放
/// 下约 380px）远小于该值，不再往上取以省显存。
pub const THUMB_MAX_EDGE: u32 = 720;

/// 缩略图：按显示上限缩小后的像素与原始尺寸。
#[derive(Clone)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// RGBA8 像素，长度 = `width * height * 4`。
    pub rgba: Vec<u8>,
    /// 原图像素尺寸（已应用 EXIF 方向，与渲染层解码原图所得一致）。
    pub source: (u32, u32),
}

impl std::fmt::Debug for Thumb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumb")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// 解码后的 RGBA8 像素（不缩放）。
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 解码并缩小到长边不超过 `max_edge`（小图不放大）。
pub fn thumbnail(bytes: &[u8], max_edge: u32) -> Result<Thumb, String> {
    let image = decode(bytes)?;
    let source = (image.width(), image.height());
    let scaled = if source.0.max(source.1) > max_edge {
        image.thumbnail(max_edge, max_edge)
    } else {
        image
    };
    let rgba = scaled.to_rgba8();
    Ok(Thumb {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        source,
    })
}

/// 解码为 RGBA8 像素（剪贴板写图用；不缩放）。
pub fn decode_rgba(bytes: &[u8]) -> Result<Pixels, String> {
    let rgba = decode(bytes)?.to_rgba8();
    Ok(Pixels { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("图片格式识别失败：{e}"))?;
    let mut decoder = reader.into_decoder().map_err(|e| format!("图片解码失败：{e}"))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("图片解码失败：{e}"))?;
    image.apply_orientation(orientation);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder, Rgb, RgbImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let pixels = RgbImage::from_pixel(width, height, Rgb([120, 80, 40]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(pixels.as_raw(), width, height, ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    /// 带 EXIF 方向标签的 JPEG（APP1 段插在 SOI 之后）。
    fn jpeg_with_orientation(width: u32, height: u32, tag: u16) -> Vec<u8> {
        let pixels = RgbImage::from_pixel(width, height, Rgb([120, 80, 40]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .write_image(pixels.as_raw(), width, height, ExtendedColorType::Rgb8)
            .unwrap();

        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&[b'I', b'I', 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]);
        app1.extend_from_slice(&1u16.to_le_bytes());
        app1.extend_from_slice(&0x0112u16.to_le_bytes());
        app1.extend_from_slice(&3u16.to_le_bytes());
        app1.extend_from_slice(&1u32.to_le_bytes());
        app1.extend_from_slice(&(u32::from(tag)).to_le_bytes());
        app1.extend_from_slice(&0u32.to_le_bytes());

        let mut out = Vec::new();
        out.extend_from_slice(&jpeg[..2]);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn thumbnail_scales_long_edge_only() {
        let thumb = thumbnail(&png(1600, 800), THUMB_MAX_EDGE).unwrap();
        assert_eq!(thumb.source, (1600, 800));
        assert_eq!((thumb.width, thumb.height), (THUMB_MAX_EDGE, THUMB_MAX_EDGE / 2));
        assert_eq!(thumb.rgba.len() as u32, THUMB_MAX_EDGE * THUMB_MAX_EDGE / 2 * 4);
    }

    #[test]
    fn thumbnail_never_upscales() {
        let thumb = thumbnail(&png(100, 50), THUMB_MAX_EDGE).unwrap();
        assert_eq!((thumb.width, thumb.height), (100, 50));
        assert_eq!(thumb.rgba.len(), 100 * 50 * 4);
    }

    #[test]
    fn thumbnail_applies_exif_orientation() {
        let thumb = thumbnail(&jpeg_with_orientation(8, 4, 6), THUMB_MAX_EDGE).unwrap();
        assert_eq!(thumb.source, (4, 8));
        assert_eq!((thumb.width, thumb.height), (4, 8));
    }

    #[test]
    fn decode_rejects_unknown_bytes() {
        assert!(thumbnail(b"not an image", THUMB_MAX_EDGE).is_err());
        assert!(decode_rgba(b"not an image").is_err());
    }

    #[test]
    fn decode_rgba_keeps_original_size() {
        let pixels = decode_rgba(&png(30, 20)).unwrap();
        assert_eq!((pixels.width, pixels.height), (30, 20));
        assert_eq!(pixels.rgba.len(), 30 * 20 * 4);
    }
}
