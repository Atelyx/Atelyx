//! 整面帧的二进制编码：共享内存槽里放的就是本模块的产物，字段顺序与类型全部显式（小端、
//! 长度前缀），不依赖自描述格式。编码按穷举写（漏一个变体即编译失败），解码按标签分派、
//! 未知标签报可读错误；布局变更必须同时升版本号，否则两端会静默错读。

use crate::frame::{FrameRect, HitCursor, HitRegion, SurfaceFrame};
use crate::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintPoint, PaintShadow, TextAlign,
    TextFamily, TextWeight,
};

const MAGIC: [u8; 4] = *b"ATXF";
const VERSION: u8 = 1;

/// 把一帧编成字节序列。
pub fn encode(frame: &SurfaceFrame) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    encode_frame(&mut out, frame);
    out
}

/// 从字节序列还原一帧：魔数 / 版本 / 长度对不上或内容截断都返回可读错误。
pub fn decode(bytes: &[u8]) -> Result<SurfaceFrame, String> {
    if bytes.len() < MAGIC.len() + 1 {
        return Err("帧编码过短".into());
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err("帧编码魔数不符".into());
    }
    if bytes[MAGIC.len()] != VERSION {
        return Err(format!("帧编码版本不符：{}", bytes[MAGIC.len()]));
    }
    let mut reader = Reader { buf: bytes, pos: MAGIC.len() + 1 };
    let frame = decode_frame(&mut reader)?;
    if reader.pos != bytes.len() {
        return Err(format!("帧编码尾部有多余字节：{}", bytes.len() - reader.pos));
    }
    Ok(frame)
}

fn encode_frame(out: &mut Vec<u8>, frame: &SurfaceFrame) {
    put_count(out, frame.commands.len());
    for command in &frame.commands {
        encode_command(out, command);
    }
    put_count(out, frame.hits.len());
    for hit in &frame.hits {
        put_str(out, &hit.id);
        put_f32(out, hit.x);
        put_f32(out, hit.y);
        put_f32(out, hit.w);
        put_f32(out, hit.h);
        put_opt_u8(out, hit.cursor.map(cursor_code));
    }
    match frame.ime {
        Some(rect) => {
            out.push(1);
            put_rect(out, &rect);
        }
        None => out.push(0),
    }
    out.push(u8::from(frame.keys));
    match frame.fps {
        Some(fps) => {
            out.push(1);
            out.extend_from_slice(&fps.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn encode_command(out: &mut Vec<u8>, command: &DrawCommand) {
    match command {
        DrawCommand::Line { x1, y1, x2, y2, stroke, width } => {
            out.push(0);
            for value in [x1, y1, x2, y2] {
                put_f32(out, *value);
            }
            encode_color(out, stroke);
            put_f32(out, *width);
        }
        DrawCommand::Rect { x, y, w, h, fill, stroke, stroke_width, corner_radius, shadow } => {
            out.push(1);
            for value in [x, y, w, h] {
                put_f32(out, *value);
            }
            put_opt(out, fill.as_ref(), encode_color);
            put_opt(out, stroke.as_ref(), encode_color);
            put_opt(out, stroke_width.as_ref(), |out, value| put_f32(out, *value));
            put_opt(out, corner_radius.as_ref(), |out, value| put_f32(out, *value));
            put_opt(out, shadow.as_ref(), encode_shadow);
        }
        DrawCommand::Path { points, closed, fill, stroke, stroke_width } => {
            out.push(2);
            put_count(out, points.len());
            for point in points {
                put_f32(out, point.x);
                put_f32(out, point.y);
            }
            out.push(u8::from(*closed));
            put_opt(out, fill.as_ref(), encode_color);
            put_opt(out, stroke.as_ref(), encode_color);
            put_opt(out, stroke_width.as_ref(), |out, value| put_f32(out, *value));
        }
        DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width } => {
            out.push(3);
            for value in [cx, cy, r] {
                put_f32(out, *value);
            }
            put_opt(out, fill.as_ref(), encode_color);
            put_opt(out, stroke.as_ref(), encode_color);
            put_opt(out, stroke_width.as_ref(), |out, value| put_f32(out, *value));
        }
        DrawCommand::Text { x, y, content, size, color, weight, family, max_width, align } => {
            out.push(4);
            put_f32(out, *x);
            put_f32(out, *y);
            put_str(out, content);
            put_f32(out, *size);
            encode_color(out, color);
            put_opt_u8(out, weight.map(weight_code));
            put_opt_u8(out, family.map(family_code));
            put_opt(out, max_width.as_ref(), |out, value| put_f32(out, *value));
            put_opt_u8(out, align.map(align_code));
        }
        DrawCommand::Image { source, x, y, w, h, fit } => {
            out.push(5);
            encode_source(out, source);
            for value in [x, y, w, h] {
                put_f32(out, *value);
            }
            put_opt_u8(out, fit.map(fit_code));
        }
    }
}

fn encode_color(out: &mut Vec<u8>, color: &PaintColor) {
    match color {
        PaintColor::Token { name } => {
            out.push(0);
            out.push(token_code(*name));
        }
        PaintColor::Rgba { rgba } => {
            out.push(1);
            out.extend_from_slice(rgba);
        }
    }
}

fn encode_shadow(out: &mut Vec<u8>, shadow: &PaintShadow) {
    for value in [shadow.offset_x, shadow.offset_y, shadow.blur] {
        put_f32(out, value);
    }
    encode_color(out, &shadow.color);
}

fn encode_source(out: &mut Vec<u8>, source: &ImageSource) {
    match source {
        ImageSource::Private { path } => {
            out.push(0);
            put_str(out, path);
        }
        ImageSource::Vault { path } => {
            out.push(1);
            put_str(out, path);
        }
        ImageSource::Url { url, local } => {
            out.push(2);
            put_str(out, url);
            put_opt_u8(out, local.map(u8::from));
        }
        ImageSource::File { path } => {
            out.push(3);
            put_str(out, path);
        }
    }
}

fn decode_frame(reader: &mut Reader<'_>) -> Result<SurfaceFrame, String> {
    let count = reader.count()?;
    let mut commands = Vec::new();
    for _ in 0..count {
        commands.push(decode_command(reader)?);
    }
    let count = reader.count()?;
    let mut hits = Vec::new();
    for _ in 0..count {
        hits.push(HitRegion {
            id: reader.string()?,
            x: reader.f32()?,
            y: reader.f32()?,
            w: reader.f32()?,
            h: reader.f32()?,
            cursor: reader.opt_u8()?.map(decode_cursor).transpose()?,
        });
    }
    let ime = match reader.bool()? {
        true => Some(reader.rect()?),
        false => None,
    };
    let keys = reader.bool()?;
    let fps = match reader.bool()? {
        true => Some(reader.u16()?),
        false => None,
    };
    Ok(SurfaceFrame { commands, hits, ime, keys, fps })
}

fn decode_command(reader: &mut Reader<'_>) -> Result<DrawCommand, String> {
    Ok(match reader.u8()? {
        0 => DrawCommand::Line {
            x1: reader.f32()?,
            y1: reader.f32()?,
            x2: reader.f32()?,
            y2: reader.f32()?,
            stroke: reader.color()?,
            width: reader.f32()?,
        },
        1 => DrawCommand::Rect {
            x: reader.f32()?,
            y: reader.f32()?,
            w: reader.f32()?,
            h: reader.f32()?,
            fill: reader.opt(|r| r.color())?,
            stroke: reader.opt(|r| r.color())?,
            stroke_width: reader.opt(|r| r.f32())?,
            corner_radius: reader.opt(|r| r.f32())?,
            shadow: reader.opt(|r| r.shadow())?,
        },
        2 => {
            let count = reader.count()?;
            let mut points = Vec::new();
            for _ in 0..count {
                points.push(PaintPoint { x: reader.f32()?, y: reader.f32()? });
            }
            DrawCommand::Path {
                points,
                closed: reader.bool()?,
                fill: reader.opt(|r| r.color())?,
                stroke: reader.opt(|r| r.color())?,
                stroke_width: reader.opt(|r| r.f32())?,
            }
        }
        3 => DrawCommand::Circle {
            cx: reader.f32()?,
            cy: reader.f32()?,
            r: reader.f32()?,
            fill: reader.opt(|r| r.color())?,
            stroke: reader.opt(|r| r.color())?,
            stroke_width: reader.opt(|r| r.f32())?,
        },
        4 => DrawCommand::Text {
            x: reader.f32()?,
            y: reader.f32()?,
            content: reader.string()?,
            size: reader.f32()?,
            color: reader.color()?,
            weight: reader.opt_u8()?.map(decode_weight).transpose()?,
            family: reader.opt_u8()?.map(decode_family).transpose()?,
            max_width: reader.opt(|r| r.f32())?,
            align: reader.opt_u8()?.map(decode_align).transpose()?,
        },
        5 => {
            let source = reader.source()?;
            DrawCommand::Image {
                source,
                x: reader.f32()?,
                y: reader.f32()?,
                w: reader.f32()?,
                h: reader.f32()?,
                fit: reader.opt_u8()?.map(decode_fit).transpose()?,
            }
        }
        other => return Err(format!("未知绘制指令：{other}")),
    })
}

pub(crate) fn token_code(token: ColorToken) -> u8 {
    match token {
        ColorToken::Text => 0,
        ColorToken::TextSecondary => 1,
        ColorToken::TextMuted => 2,
        ColorToken::Accent => 3,
        ColorToken::AccentSoft => 4,
        ColorToken::Danger => 5,
        ColorToken::DangerSoft => 6,
        ColorToken::Success => 7,
        ColorToken::SuccessSoft => 8,
        ColorToken::Warning => 9,
        ColorToken::WarningSoft => 10,
        ColorToken::Surface => 11,
        ColorToken::SurfaceMuted => 12,
        ColorToken::SurfaceSunken => 13,
        ColorToken::Hover => 14,
        ColorToken::Border => 15,
        ColorToken::BorderStrong => 16,
        ColorToken::Transparent => 17,
    }
}

pub(crate) fn decode_token(code: u8) -> Result<ColorToken, String> {
    Ok(match code {
        0 => ColorToken::Text,
        1 => ColorToken::TextSecondary,
        2 => ColorToken::TextMuted,
        3 => ColorToken::Accent,
        4 => ColorToken::AccentSoft,
        5 => ColorToken::Danger,
        6 => ColorToken::DangerSoft,
        7 => ColorToken::Success,
        8 => ColorToken::SuccessSoft,
        9 => ColorToken::Warning,
        10 => ColorToken::WarningSoft,
        11 => ColorToken::Surface,
        12 => ColorToken::SurfaceMuted,
        13 => ColorToken::SurfaceSunken,
        14 => ColorToken::Hover,
        15 => ColorToken::Border,
        16 => ColorToken::BorderStrong,
        17 => ColorToken::Transparent,
        other => return Err(format!("未知主题色：{other}")),
    })
}

fn weight_code(weight: TextWeight) -> u8 {
    match weight {
        TextWeight::Normal => 0,
        TextWeight::Medium => 1,
        TextWeight::SemiBold => 2,
    }
}

fn decode_weight(code: u8) -> Result<TextWeight, String> {
    Ok(match code {
        0 => TextWeight::Normal,
        1 => TextWeight::Medium,
        2 => TextWeight::SemiBold,
        other => return Err(format!("未知字重：{other}")),
    })
}

fn family_code(family: TextFamily) -> u8 {
    match family {
        TextFamily::Ui => 0,
        TextFamily::Mono => 1,
    }
}

fn decode_family(code: u8) -> Result<TextFamily, String> {
    Ok(match code {
        0 => TextFamily::Ui,
        1 => TextFamily::Mono,
        other => return Err(format!("未知字体族：{other}")),
    })
}

fn align_code(align: TextAlign) -> u8 {
    match align {
        TextAlign::Start => 0,
        TextAlign::Center => 1,
        TextAlign::End => 2,
    }
}

fn decode_align(code: u8) -> Result<TextAlign, String> {
    Ok(match code {
        0 => TextAlign::Start,
        1 => TextAlign::Center,
        2 => TextAlign::End,
        other => return Err(format!("未知对齐：{other}")),
    })
}

fn fit_code(fit: ImageFit) -> u8 {
    match fit {
        ImageFit::Contain => 0,
        ImageFit::Cover => 1,
        ImageFit::Fill => 2,
    }
}

fn decode_fit(code: u8) -> Result<ImageFit, String> {
    Ok(match code {
        0 => ImageFit::Contain,
        1 => ImageFit::Cover,
        2 => ImageFit::Fill,
        other => return Err(format!("未知填充方式：{other}")),
    })
}

fn cursor_code(cursor: HitCursor) -> u8 {
    match cursor {
        HitCursor::Default => 0,
        HitCursor::Pointer => 1,
        HitCursor::Text => 2,
        HitCursor::Grab => 3,
        HitCursor::Grabbing => 4,
        HitCursor::Crosshair => 5,
        HitCursor::NotAllowed => 6,
        HitCursor::ResizeHorizontal => 7,
        HitCursor::ResizeVertical => 8,
    }
}

fn decode_cursor(code: u8) -> Result<HitCursor, String> {
    Ok(match code {
        0 => HitCursor::Default,
        1 => HitCursor::Pointer,
        2 => HitCursor::Text,
        3 => HitCursor::Grab,
        4 => HitCursor::Grabbing,
        5 => HitCursor::Crosshair,
        6 => HitCursor::NotAllowed,
        7 => HitCursor::ResizeHorizontal,
        8 => HitCursor::ResizeVertical,
        other => return Err(format!("未知光标形状：{other}")),
    })
}

fn put_count(out: &mut Vec<u8>, count: usize) {
    out.extend_from_slice(&(count as u32).to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_count(out, value.len());
    out.extend_from_slice(value.as_bytes());
}

fn put_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_rect(out: &mut Vec<u8>, rect: &FrameRect) {
    for value in [rect.x, rect.y, rect.w, rect.h] {
        put_f32(out, value);
    }
}

fn put_opt_u8(out: &mut Vec<u8>, value: Option<u8>) {
    match value {
        Some(code) => {
            out.push(1);
            out.push(code);
        }
        None => out.push(0),
    }
}

fn put_opt<T>(out: &mut Vec<u8>, value: Option<&T>, encode: impl Fn(&mut Vec<u8>, &T)) {
    match value {
        Some(value) => {
            out.push(1);
            encode(out, value);
        }
        None => out.push(0),
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(len).ok_or_else(|| "帧编码长度溢出".to_string())?;
        if end > self.buf.len() {
            return Err(format!("帧编码截断：偏移 {} 起需要 {len} 字节", self.pos));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!("布尔值非法：{other}")),
        }
    }

    fn u16(&mut self) -> Result<u16, String> {
        let raw = self.take(2)?;
        Ok(u16::from_le_bytes([raw[0], raw[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let raw = self.take(4)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// 集合长度：每个元素至少占一个字节，声明数超过剩余字节数即可判非法；不按声明数预分配，
    /// 错位内容无法撑出一次巨型分配。
    fn count(&mut self) -> Result<usize, String> {
        let count = self.u32()? as usize;
        let remaining = self.buf.len() - self.pos;
        if count > remaining {
            return Err(format!("帧编码集合长度非法：{count}（剩余 {remaining} 字节）"));
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        if len > self.buf.len() - self.pos {
            return Err(format!("帧内字符串长度越界：{len}"));
        }
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).map_err(|e| format!("帧内字符串不是 UTF-8：{e}"))
    }

    fn opt_u8(&mut self) -> Result<Option<u8>, String> {
        match self.bool()? {
            true => Ok(Some(self.u8()?)),
            false => Ok(None),
        }
    }

    fn opt<T>(&mut self, decode: impl Fn(&mut Self) -> Result<T, String>) -> Result<Option<T>, String> {
        match self.bool()? {
            true => Ok(Some(decode(self)?)),
            false => Ok(None),
        }
    }

    fn rect(&mut self) -> Result<FrameRect, String> {
        Ok(FrameRect {
            x: self.f32()?,
            y: self.f32()?,
            w: self.f32()?,
            h: self.f32()?,
        })
    }

    fn color(&mut self) -> Result<PaintColor, String> {
        Ok(match self.u8()? {
            0 => PaintColor::Token { name: decode_token(self.u8()?)? },
            1 => {
                let raw = self.take(4)?;
                PaintColor::Rgba { rgba: [raw[0], raw[1], raw[2], raw[3]] }
            }
            other => return Err(format!("未知颜色形态：{other}")),
        })
    }

    fn shadow(&mut self) -> Result<PaintShadow, String> {
        Ok(PaintShadow {
            offset_x: self.f32()?,
            offset_y: self.f32()?,
            blur: self.f32()?,
            color: self.color()?,
        })
    }

    fn source(&mut self) -> Result<ImageSource, String> {
        Ok(match self.u8()? {
            0 => ImageSource::Private { path: self.string()? },
            1 => ImageSource::Vault { path: self.string()? },
            2 => ImageSource::Url { url: self.string()?, local: self.opt_u8()?.map(|v| v != 0) },
            3 => ImageSource::File { path: self.string()? },
            other => return Err(format!("未知图片来源：{other}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SurfaceFrame {
        SurfaceFrame {
            commands: vec![
                DrawCommand::Line {
                    x1: 1.0,
                    y1: 2.0,
                    x2: 3.0,
                    y2: 4.0,
                    stroke: PaintColor::Rgba { rgba: [1, 2, 3, 255] },
                    width: 2.0,
                },
                DrawCommand::Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 5.0,
                    fill: Some(PaintColor::Token { name: ColorToken::AccentSoft }),
                    stroke: Some(PaintColor::Token { name: ColorToken::Border }),
                    stroke_width: Some(1.5),
                    corner_radius: None,
                    shadow: Some(PaintShadow {
                        offset_x: 0.0,
                        offset_y: 2.0,
                        blur: 6.0,
                        color: PaintColor::Rgba { rgba: [0, 0, 0, 32] },
                    }),
                },
                DrawCommand::Path {
                    points: vec![PaintPoint { x: 0.0, y: 1.0 }, PaintPoint { x: 2.0, y: 3.0 }],
                    closed: true,
                    fill: Some(PaintColor::Token { name: ColorToken::Surface }),
                    stroke: None,
                    stroke_width: None,
                },
                DrawCommand::Circle {
                    cx: 5.0,
                    cy: 6.0,
                    r: 2.0,
                    fill: None,
                    stroke: Some(PaintColor::Rgba { rgba: [9, 8, 7, 255] }),
                    stroke_width: Some(1.0),
                },
                DrawCommand::Text {
                    x: 1.0,
                    y: 2.0,
                    content: "计数：中".into(),
                    size: 14.0,
                    color: PaintColor::Token { name: ColorToken::Text },
                    weight: Some(TextWeight::SemiBold),
                    family: Some(TextFamily::Mono),
                    max_width: Some(120.0),
                    align: Some(TextAlign::Center),
                },
                DrawCommand::Image {
                    source: ImageSource::Url { url: "https://example.com/a.png".into(), local: Some(true) },
                    x: 0.0,
                    y: 0.0,
                    w: 8.0,
                    h: 8.0,
                    fit: Some(ImageFit::Cover),
                },
            ],
            hits: vec![
                HitRegion { id: "board".into(), x: 0.0, y: 0.0, w: 100.0, h: 100.0, cursor: None },
                HitRegion { id: "btn".into(), x: 1.0, y: 2.0, w: 3.0, h: 4.0, cursor: Some(HitCursor::Pointer) },
            ],
            ime: Some(FrameRect { x: 4.0, y: 5.0, w: 2.0, h: 18.0 }),
            keys: true,
            fps: Some(60),
        }
    }

    #[test]
    fn full_frame_roundtrips_byte_exact() {
        let frame = sample();
        let bytes = encode(&frame);
        assert_eq!(decode(&bytes).unwrap(), frame);
        assert_eq!(encode(&decode(&bytes).unwrap()), bytes, "同内容编码须稳定");
    }

    #[test]
    fn truncation_and_trailing_bytes_are_rejected() {
        let bytes = encode(&sample());
        for cut in [0, 1, 5, bytes.len() / 2, bytes.len() - 1] {
            assert!(decode(&bytes[..cut]).is_err(), "截断到 {cut} 应报错");
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(decode(&extra).unwrap_err().contains("多余"));
    }

    #[test]
    fn unknown_variant_and_bad_lengths_are_readable_errors() {
        let mut bytes = encode(&sample());
        let tag_at = MAGIC.len() + 1 + 4;
        bytes[tag_at] = 9;
        assert!(decode(&bytes).unwrap_err().contains("未知绘制指令"));

        let mut huge = encode(&SurfaceFrame::default());
        let count_at = MAGIC.len() + 1;
        huge[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&huge).unwrap_err().contains("集合长度非法"));
    }

    /// 过线布局冻结：标签序、字段顺序与数值编码一变即须升版本，否则旧插件 / 旧宿主会静默错读。
    #[test]
    fn wire_layout_is_frozen() {
        let frame = SurfaceFrame {
            commands: vec![
                DrawCommand::Rect {
                    x: 1.0,
                    y: 2.0,
                    w: 3.0,
                    h: 4.0,
                    fill: Some(PaintColor::Token { name: ColorToken::Accent }),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: Some(6.0),
                    shadow: None,
                },
                DrawCommand::Text {
                    x: 0.0,
                    y: 0.0,
                    content: "中".into(),
                    size: 14.0,
                    color: PaintColor::Rgba { rgba: [1, 2, 3, 255] },
                    weight: None,
                    family: None,
                    max_width: None,
                    align: None,
                },
            ],
            hits: vec![HitRegion {
                id: "a".into(),
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
                cursor: Some(HitCursor::Text),
            }],
            ime: None,
            keys: false,
            fps: Some(60),
        };
        let bytes = encode(&frame);
        assert_eq!(
            hex(&bytes),
            concat!(
                "415458460102000000010000803f00000040000040400000804001000300",
                "00010000c0400004000000000000000003000000e4b8ad00006041010102",
                "03ff00000000010000000100000061000000000000000000002041000020",
                "4101020000013c00",
            )
        );
        assert_eq!(decode(&bytes).unwrap(), frame);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn empty_frame_is_valid() {
        let frame = SurfaceFrame::default();
        assert_eq!(decode(&encode(&frame)).unwrap(), frame);
    }

    #[test]
    fn every_token_and_image_source_roundtrips() {
        let tokens = [
            ColorToken::Text,
            ColorToken::TextSecondary,
            ColorToken::TextMuted,
            ColorToken::Accent,
            ColorToken::AccentSoft,
            ColorToken::Danger,
            ColorToken::DangerSoft,
            ColorToken::Success,
            ColorToken::SuccessSoft,
            ColorToken::Warning,
            ColorToken::WarningSoft,
            ColorToken::Surface,
            ColorToken::SurfaceMuted,
            ColorToken::SurfaceSunken,
            ColorToken::Hover,
            ColorToken::Border,
            ColorToken::BorderStrong,
            ColorToken::Transparent,
        ];
        for (index, token) in tokens.into_iter().enumerate() {
            assert_eq!(decode_token(token_code(token)).unwrap(), token);
            assert_eq!(token_code(token) as usize, index);
        }
        let sources = [
            ImageSource::Private { path: "a.png".into() },
            ImageSource::Vault { path: "b.png".into() },
            ImageSource::Url { url: "https://example.com".into(), local: None },
            ImageSource::File { path: "/tmp/c.png".into() },
        ];
        let mut out = Vec::new();
        for source in &sources {
            encode_source(&mut out, source);
        }
        let mut reader = Reader { buf: &out, pos: 0 };
        for source in &sources {
            assert_eq!(&reader.source().unwrap(), source);
        }
    }
}
