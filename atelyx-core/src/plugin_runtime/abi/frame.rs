//! ABI 帧到协议帧的一次性复制：指令、命中区与输入法矩形在这里转成宿主协议类型。
//! 返回后插件即可复用自身的指令与字符串内存，后续绘制管线不区分帧的来源。

use atelyx_plugin_abi as abi;
use atelyx_plugin_protocol::frame::{
    FrameRect, FrameSize, HitCursor, HitRegion, PopupCapture, PopupLayer, SurfaceFrame,
    SurfacePopup,
};
use atelyx_plugin_protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, PaintPoint, PaintShadow, TextAlign,
    TextFamily, TextLayout, TextShaping, TextWeight, TextWrap,
};

/// 复制一帧 ABI 内容成协议帧。
///
/// # Safety
/// `frame` 内的指针必须指向本进程内当前有效的指令 / 命中区 / 字符串；本函数在返回前复制全部内容。
pub(super) unsafe fn frame_from_abi(frame: &abi::AbiFrame) -> Result<SurfaceFrame, String> {
    let commands = unsafe { commands_from_abi(frame.commands, frame.commands_len) }?;
    let hits = unsafe { hits_from_abi(frame.hits, frame.hits_len) };
    let popup = if frame.has_popup != 0 {
        Some(SurfacePopup {
            rect: frame_rect(frame.popup_rect),
            capture: popup_capture_from_code(frame.popup_capture),
            layer: popup_layer_from_code(frame.popup_layer),
            order: u16::try_from(frame.popup_order).unwrap_or(u16::MAX),
            commands: unsafe { commands_from_abi(frame.popup_commands, frame.popup_commands_len) }?,
            hits: unsafe { hits_from_abi(frame.popup_hits, frame.popup_hits_len) },
            ime: (frame.has_popup_ime != 0).then(|| frame_rect(frame.popup_ime)),
        })
    } else {
        None
    };
    Ok(SurfaceFrame {
        size: (frame.has_size != 0).then_some(FrameSize { w: frame.layout_w, h: frame.layout_h }),
        commands,
        hits,
        ime: (frame.has_ime != 0).then(|| frame_rect(frame.ime)),
        keys: frame.keys != 0,
        fps: u16::try_from(frame.fps).ok().filter(|fps| *fps > 0),
        popup,
    })
}

unsafe fn commands_from_abi(
    commands: *const abi::AbiCommand,
    len: usize,
) -> Result<Vec<DrawCommand>, String> {
    if commands.is_null() || len == 0 {
        return Ok(Vec::new());
    }
    let raw = unsafe { std::slice::from_raw_parts(commands, len) };
    raw.iter()
        .map(|raw_command| unsafe { command_from_abi(raw_command) })
        .collect()
}

unsafe fn hits_from_abi(hits: *const abi::AbiHit, len: usize) -> Vec<HitRegion> {
    if hits.is_null() || len == 0 {
        return Vec::new();
    }
    let raw = unsafe { std::slice::from_raw_parts(hits, len) };
    raw.iter()
        .map(|hit| HitRegion {
            id: copy_str(hit.id),
            x: hit.rect.x,
            y: hit.rect.y,
            w: hit.rect.w,
            h: hit.rect.h,
            cursor: cursor_from_code(hit.cursor),
        })
        .collect()
}

fn frame_rect(value: abi::AbiRect) -> FrameRect {
    FrameRect { x: value.x, y: value.y, w: value.w, h: value.h }
}

unsafe fn command_from_abi(value: &abi::AbiCommand) -> Result<DrawCommand, String> {
    Ok(match value.kind {
        abi::CMD_LINE => DrawCommand::Line {
            x1: value.rect.x,
            y1: value.rect.y,
            x2: value.line_x2,
            y2: value.line_y2,
            stroke: paint_color(value.rect.stroke),
            width: stroke_width(value.rect.stroke_width),
            dash: dash_from_abi(value.line_dash_on, value.line_dash_off),
        },
        abi::CMD_RECT => DrawCommand::Rect {
            x: value.rect.x,
            y: value.rect.y,
            w: value.rect.w,
            h: value.rect.h,
            fill: (value.rect.has_fill != 0).then(|| paint_color(value.rect.fill)),
            stroke: (value.rect.has_stroke != 0).then(|| paint_color(value.rect.stroke)),
            stroke_width: Some(stroke_width(value.rect.stroke_width)),
            corner_radius: (value.rect.radius > 0.0).then_some(value.rect.radius),
            shadow: (value.shadow.enabled != 0).then(|| PaintShadow {
                offset_x: value.shadow.offset_x,
                offset_y: value.shadow.offset_y,
                blur: value.shadow.blur,
                color: paint_color(value.shadow.color),
            }),
        },
        abi::CMD_PATH => {
            let points = if value.points.is_null() || value.points_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(value.points, value.points_len) }
                    .iter()
                    .map(|point| PaintPoint { x: point.x, y: point.y })
                    .collect()
            };
            DrawCommand::Path {
                points,
                closed: value.closed != 0,
                fill: (value.rect.has_fill != 0).then(|| paint_color(value.rect.fill)),
                stroke: (value.rect.has_stroke != 0).then(|| paint_color(value.rect.stroke)),
                stroke_width: Some(stroke_width(value.rect.stroke_width)),
            }
        }
        abi::CMD_CIRCLE => DrawCommand::Circle {
            cx: value.circle_cx,
            cy: value.circle_cy,
            r: value.circle_r,
            fill: (value.rect.has_fill != 0).then(|| paint_color(value.rect.fill)),
            stroke: (value.rect.has_stroke != 0).then(|| paint_color(value.rect.stroke)),
            stroke_width: Some(stroke_width(value.rect.stroke_width)),
        },
        abi::CMD_TEXT => DrawCommand::Text {
            x: value.rect.x,
            y: value.rect.y,
            content: copy_str(value.text),
            size: value.text_size,
            color: paint_color(value.text_color),
            weight: weight_from_code(value.text_weight),
            family: family_from_code(value.text_family),
            family_name: family_name_from_abi(value.text_family_name),
            italic: value.text_italic != 0,
            max_width: (value.text_max_width > 0.0).then_some(value.text_max_width),
            align: align_from_code(value.text_align),
            layout: layout_from_abi(value),
        },
        abi::CMD_IMAGE => DrawCommand::Image {
            source: image_source(
                value.image_source,
                value.image_path,
                value.image_local != 0,
                value.image_max_edge,
            )?,
            x: value.rect.x,
            y: value.rect.y,
            w: value.rect.w,
            h: value.rect.h,
            fit: fit_from_code(value.image_fit),
        },
        other => return Err(format!("未知绘制指令：{other}")),
    })
}

/// 虚线缺省口径：非法段长归一成实线（不因装饰参数丢整帧）。
fn dash_from_abi(on: f32, off: f32) -> Option<[f32; 2]> {
    (on > 0.0 && off >= 0.0).then_some([on, off])
}

/// 文字排版缺省口径：三项都是默认值 = 不声明 `layout`。
fn layout_from_abi(value: &abi::AbiCommand) -> Option<TextLayout> {
    let line_height = (value.text_line_height > 0.0).then_some(value.text_line_height);
    let wrap = match value.text_wrap {
        abi::TEXT_WRAP_WORD_OR_GLYPH => Some(TextWrap::WordOrGlyph),
        _ => None,
    };
    let shaping = shaping_from_code(value.text_shaping);
    (line_height.is_some() || wrap.is_some() || shaping.is_some()).then_some(TextLayout {
        line_height,
        wrap,
        shaping,
    })
}

fn shaping_from_code(code: u32) -> Option<TextShaping> {
    match code {
        abi::TEXT_SHAPING_BASIC => Some(TextShaping::Basic),
        abi::TEXT_SHAPING_ADVANCED => Some(TextShaping::Advanced),
        _ => None,
    }
}

fn popup_capture_from_code(code: u32) -> PopupCapture {
    if code == abi::POPUP_CAPTURE_ALL {
        PopupCapture::All
    } else {
        PopupCapture::Inside
    }
}

fn popup_layer_from_code(code: u32) -> PopupLayer {
    if code == abi::POPUP_LAYER_WINDOW {
        PopupLayer::Window
    } else {
        PopupLayer::Panel
    }
}

fn image_source(
    kind: u32,
    path: abi::AbiStr,
    local: bool,
    max_edge: u32,
) -> Result<ImageSource, String> {
    let path = copy_str(path);
    Ok(match kind {
        abi::IMAGE_PRIVATE => ImageSource::Private { path },
        abi::IMAGE_VAULT => {
            ImageSource::Vault { path, max_edge: (max_edge != 0).then_some(max_edge) }
        }
        abi::IMAGE_URL => ImageSource::Url { url: path, local: local.then_some(true) },
        abi::IMAGE_FILE => ImageSource::File { path },
        other => return Err(format!("未知图片来源：{other}")),
    })
}

fn paint_color(value: abi::AbiColor) -> PaintColor {
    if value.kind == abi::COLOR_TOKEN {
        PaintColor::Token { name: token_from_code(value.token).unwrap_or(ColorToken::Text) }
    } else {
        PaintColor::Rgba { rgba: [value.r, value.g, value.b, value.a] }
    }
}

/// 描边宽度缺省口径：未填或非正数按 1 个逻辑像素（与协议侧缺省一致）。
fn stroke_width(value: f32) -> f32 {
    if value > 0.0 {
        value
    } else {
        1.0
    }
}

/// 复制借用字符串；空指针 / 零长度按空串处理。
pub(super) fn copy_str(value: abi::AbiStr) -> String {
    if value.is_empty() {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.data, value.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// 显式字体族名：空串 = 未指定。
fn family_name_from_abi(value: abi::AbiStr) -> Option<String> {
    let name = copy_str(value);
    (!name.is_empty()).then_some(name)
}

fn token_from_code(code: u32) -> Option<ColorToken> {
    Some(match code {
        abi::TOKEN_TEXT => ColorToken::Text,
        abi::TOKEN_TEXT_SECONDARY => ColorToken::TextSecondary,
        abi::TOKEN_TEXT_MUTED => ColorToken::TextMuted,
        abi::TOKEN_ACCENT => ColorToken::Accent,
        abi::TOKEN_ACCENT_SOFT => ColorToken::AccentSoft,
        abi::TOKEN_DANGER => ColorToken::Danger,
        abi::TOKEN_DANGER_SOFT => ColorToken::DangerSoft,
        abi::TOKEN_SUCCESS => ColorToken::Success,
        abi::TOKEN_SUCCESS_SOFT => ColorToken::SuccessSoft,
        abi::TOKEN_WARNING => ColorToken::Warning,
        abi::TOKEN_WARNING_SOFT => ColorToken::WarningSoft,
        abi::TOKEN_SURFACE => ColorToken::Surface,
        abi::TOKEN_SURFACE_MUTED => ColorToken::SurfaceMuted,
        abi::TOKEN_SURFACE_SUNKEN => ColorToken::SurfaceSunken,
        abi::TOKEN_HOVER => ColorToken::Hover,
        abi::TOKEN_BORDER => ColorToken::Border,
        abi::TOKEN_BORDER_STRONG => ColorToken::BorderStrong,
        abi::TOKEN_BACKGROUND => ColorToken::Background,
        abi::TOKEN_BORDER_SUBTLE => ColorToken::BorderSubtle,
        abi::TOKEN_SELECTION => ColorToken::Selection,
        abi::TOKEN_SCRIM => ColorToken::Scrim,
        abi::TOKEN_TRANSPARENT => ColorToken::Transparent,
        abi::TOKEN_SURFACE_OVERLAY => ColorToken::SurfaceOverlay,
        _ => return None,
    })
}

fn weight_from_code(code: u32) -> Option<TextWeight> {
    match code {
        abi::WEIGHT_MEDIUM => Some(TextWeight::Medium),
        abi::WEIGHT_SEMI_BOLD => Some(TextWeight::SemiBold),
        abi::WEIGHT_BOLD => Some(TextWeight::Bold),
        _ => None,
    }
}

fn family_from_code(code: u32) -> Option<TextFamily> {
    match code {
        abi::FAMILY_MONO => Some(TextFamily::Mono),
        _ => None,
    }
}

fn align_from_code(code: u32) -> Option<TextAlign> {
    match code {
        abi::ALIGN_CENTER => Some(TextAlign::Center),
        abi::ALIGN_END => Some(TextAlign::End),
        _ => None,
    }
}

fn fit_from_code(code: u32) -> Option<ImageFit> {
    match code {
        abi::FIT_CONTAIN => Some(ImageFit::Contain),
        abi::FIT_COVER => Some(ImageFit::Cover),
        _ => None,
    }
}

fn cursor_from_code(code: u32) -> Option<HitCursor> {
    match code {
        abi::CURSOR_POINTER => Some(HitCursor::Pointer),
        abi::CURSOR_TEXT => Some(HitCursor::Text),
        abi::CURSOR_GRAB => Some(HitCursor::Grab),
        abi::CURSOR_GRABBING => Some(HitCursor::Grabbing),
        abi::CURSOR_CROSSHAIR => Some(HitCursor::Crosshair),
        abi::CURSOR_NOT_ALLOWED => Some(HitCursor::NotAllowed),
        abi::CURSOR_RESIZE_H => Some(HitCursor::ResizeHorizontal),
        abi::CURSOR_RESIZE_V => Some(HitCursor::ResizeVertical),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(kind: u32) -> abi::AbiCommand {
        abi::AbiCommand {
            kind,
            rect: abi::AbiRect::empty_at(0.0, 0.0),
            shadow: abi::AbiShadow::NONE,
            line_x2: 0.0,
            line_y2: 0.0,
            circle_cx: 0.0,
            circle_cy: 0.0,
            circle_r: 0.0,
            points: std::ptr::null(),
            points_len: 0,
            closed: 0,
            text: abi::AbiStr::EMPTY,
            text_size: 0.0,
            text_color: abi::AbiColor::TRANSPARENT,
            text_weight: abi::WEIGHT_NORMAL,
            text_family: abi::FAMILY_UI,
            text_align: abi::ALIGN_START,
            text_italic: 0,
            text_max_width: 0.0,
            image_source: 0,
            image_path: abi::AbiStr::EMPTY,
            image_local: 0,
            image_fit: abi::FIT_FILL,
            text_line_height: 0.0,
            text_wrap: abi::TEXT_WRAP_WORD,
            text_shaping: abi::TEXT_SHAPING_AUTO,
            line_dash_on: 0.0,
            line_dash_off: 0.0,
            image_max_edge: 0,
            text_family_name: abi::AbiStr::EMPTY,
        }
    }

    fn frame_with(commands: *const abi::AbiCommand, commands_len: usize) -> abi::AbiFrame {
        abi::AbiFrame {
            commands,
            commands_len,
            hits: std::ptr::null(),
            hits_len: 0,
            has_ime: 0,
            ime: abi::AbiRect::empty_at(0.0, 0.0),
            keys: 0,
            fps: 0,
            has_size: 0,
            layout_w: 0.0,
            layout_h: 0.0,
            has_popup: 0,
            popup_rect: abi::AbiRect::empty_at(0.0, 0.0),
            popup_capture: abi::POPUP_CAPTURE_INSIDE,
            popup_layer: abi::POPUP_LAYER_PANEL,
            popup_order: 0,
            popup_commands: std::ptr::null(),
            popup_commands_len: 0,
            popup_hits: std::ptr::null(),
            popup_hits_len: 0,
            has_popup_ime: 0,
            popup_ime: abi::AbiRect::empty_at(0.0, 0.0),
        }
    }

    #[test]
    fn frame_copy_owns_strings_and_geometry() {
        let text = String::from("hello");
        let family_name = String::from("Noto Sans");
        let id = String::from("button");
        let popup_id = String::from("popup-row");
        let path = String::from("note.md");
        let points = [abi::AbiPoint { x: 1.0, y: 2.0 }, abi::AbiPoint { x: 3.0, y: 4.0 }];
        let mut text_command = base(abi::CMD_TEXT);
        text_command.rect = abi::AbiRect::empty_at(1.0, 2.0);
        text_command.text = abi::AbiStr::new(&text);
        text_command.text_size = 14.0;
        text_command.text_color = abi::AbiColor::token(abi::TOKEN_TEXT);
        text_command.text_weight = abi::WEIGHT_SEMI_BOLD;
        text_command.text_family = abi::FAMILY_MONO;
        text_command.text_family_name = abi::AbiStr::new(&family_name);
        text_command.text_align = abi::ALIGN_CENTER;
        text_command.text_max_width = 120.0;
        text_command.text_line_height = 1.5;
        text_command.text_wrap = abi::TEXT_WRAP_WORD_OR_GLYPH;
        text_command.text_shaping = abi::TEXT_SHAPING_ADVANCED;
        let mut path_command = base(abi::CMD_PATH);
        path_command.points = points.as_ptr();
        path_command.points_len = points.len();
        path_command.closed = 1;
        let mut image_command = base(abi::CMD_IMAGE);
        image_command.rect =
            abi::AbiRect::filled(0.0, 0.0, 32.0, 32.0, 4.0, abi::AbiColor::TRANSPARENT);
        image_command.image_source = abi::IMAGE_VAULT;
        image_command.image_path = abi::AbiStr::new(&path);
        image_command.image_fit = abi::FIT_COVER;
        image_command.image_max_edge = 320;
        let commands = [text_command, path_command, image_command];
        let hits = [abi::AbiHit {
            id: abi::AbiStr::new(&id),
            rect: abi::AbiRect::filled(0.0, 0.0, 10.0, 10.0, 0.0, abi::AbiColor::TRANSPARENT),
            cursor: abi::CURSOR_POINTER,
        }];
        let mut popup_command = base(abi::CMD_RECT);
        popup_command.rect =
            abi::AbiRect::filled(10.0, 20.0, 30.0, 40.0, 0.0, abi::AbiColor::rgba(1, 2, 3, 4));
        let popup_commands = [popup_command];
        let popup_hits = [abi::AbiHit {
            id: abi::AbiStr::new(&popup_id),
            rect: abi::AbiRect::filled(10.0, 20.0, 30.0, 40.0, 0.0, abi::AbiColor::TRANSPARENT),
            cursor: abi::CURSOR_DEFAULT,
        }];
        let frame = abi::AbiFrame {
            hits: hits.as_ptr(),
            hits_len: hits.len(),
            keys: 1,
            fps: 30,
            has_size: 1,
            layout_w: 120.0,
            layout_h: 80.0,
            has_popup: 1,
            popup_rect: abi::AbiRect::filled(
                5.0,
                6.0,
                100.0,
                120.0,
                0.0,
                abi::AbiColor::TRANSPARENT,
            ),
            popup_capture: abi::POPUP_CAPTURE_ALL,
            popup_commands: popup_commands.as_ptr(),
            popup_commands_len: popup_commands.len(),
            popup_hits: popup_hits.as_ptr(),
            popup_hits_len: popup_hits.len(),
            has_popup_ime: 1,
            popup_ime: abi::AbiRect::filled(
                7.0,
                8.0,
                9.0,
                18.0,
                0.0,
                abi::AbiColor::TRANSPARENT,
            ),
            ..frame_with(commands.as_ptr(), commands.len())
        };
        let owned = unsafe { frame_from_abi(&frame) }.unwrap();
        // 复制后释放源字符串：断言只能靠宿主自己持有的副本成立。
        drop(text);
        drop(family_name);
        drop(id);
        drop(popup_id);
        drop(path);
        let _ = (&commands, &hits, &popup_commands, &popup_hits, &points);
        assert_eq!(owned.commands.len(), 3);
        let DrawCommand::Text {
            content, color, weight, family, family_name, max_width, align, layout, ..
        } = &owned.commands[0]
        else {
            panic!("第一条应是文字");
        };
        assert_eq!(content, "hello");
        assert_eq!(*color, PaintColor::Token { name: ColorToken::Text });
        assert_eq!(*weight, Some(TextWeight::SemiBold));
        assert_eq!(*family, Some(TextFamily::Mono));
        assert_eq!(*family_name, Some("Noto Sans".to_string()));
        assert_eq!(*max_width, Some(120.0));
        assert_eq!(*align, Some(TextAlign::Center));
        assert_eq!(
            *layout,
            Some(TextLayout {
                line_height: Some(1.5),
                wrap: Some(TextWrap::WordOrGlyph),
                shaping: Some(TextShaping::Advanced),
            })
        );
        let DrawCommand::Path { points, closed, .. } = &owned.commands[1] else {
            panic!("第二条应是路径");
        };
        assert_eq!(points.len(), 2);
        assert!(*closed);
        let DrawCommand::Image { source, fit, .. } = &owned.commands[2] else {
            panic!("第三条应是图片");
        };
        assert_eq!(*source, ImageSource::Vault { path: "note.md".into(), max_edge: Some(320) });
        assert_eq!(*fit, Some(ImageFit::Cover));
        assert_eq!(owned.hits[0].id, "button");
        assert_eq!(owned.hits[0].cursor, Some(HitCursor::Pointer));
        assert!(owned.keys);
        assert_eq!(owned.fps, Some(30));
        assert_eq!(owned.size, Some(FrameSize { w: 120.0, h: 80.0 }));
        let popup = owned.popup.as_ref().expect("应带浮层");
        assert_eq!(popup.rect, FrameRect { x: 5.0, y: 6.0, w: 100.0, h: 120.0 });
        assert_eq!(popup.capture, PopupCapture::All);
        assert_eq!(popup.commands.len(), 1);
        assert_eq!(popup.hits[0].id, "popup-row");
        assert_eq!(popup.ime, Some(FrameRect { x: 7.0, y: 8.0, w: 9.0, h: 18.0 }));
    }

    #[test]
    fn frame_copy_covers_geometry_shadow_and_ime() {
        let mut line = base(abi::CMD_LINE);
        line.rect = abi::AbiRect::empty_at(1.0, 2.0);
        line.rect.stroke = abi::AbiColor::rgba(9, 8, 7, 255);
        line.rect.has_stroke = 1;
        line.rect.stroke_width = 2.0;
        line.line_x2 = 3.0;
        line.line_y2 = 4.0;
        line.line_dash_on = 4.0;
        line.line_dash_off = 2.0;
        let mut area = base(abi::CMD_RECT);
        area.rect = abi::AbiRect::filled(5.0, 6.0, 7.0, 8.0, 2.0, abi::AbiColor::rgba(1, 2, 3, 4));
        area.rect.stroke = abi::AbiColor::token(abi::TOKEN_BORDER);
        area.rect.has_stroke = 1;
        area.rect.stroke_width = 0.0;
        area.shadow = abi::AbiShadow {
            offset_x: 1.0,
            offset_y: 2.0,
            blur: 3.0,
            color: abi::AbiColor::rgba(0, 0, 0, 40),
            enabled: 1,
        };
        let mut circle = base(abi::CMD_CIRCLE);
        circle.circle_cx = 10.0;
        circle.circle_cy = 11.0;
        circle.circle_r = 12.0;
        circle.rect.has_fill = 1;
        circle.rect.fill = abi::AbiColor::token(abi::TOKEN_ACCENT);
        let commands = [line, area, circle];
        let frame = abi::AbiFrame {
            has_ime: 1,
            ime: abi::AbiRect::filled(4.0, 5.0, 6.0, 18.0, 0.0, abi::AbiColor::TRANSPARENT),
            ..frame_with(commands.as_ptr(), commands.len())
        };
        let owned = unsafe { frame_from_abi(&frame) }.unwrap();
        let DrawCommand::Line { x1, y1, x2, y2, stroke, width, dash } = &owned.commands[0] else {
            panic!("第一条应是线");
        };
        assert_eq!((*x1, *y1, *x2, *y2), (1.0, 2.0, 3.0, 4.0));
        assert_eq!(*stroke, PaintColor::Rgba { rgba: [9, 8, 7, 255] });
        assert_eq!(*width, 2.0);
        assert_eq!(*dash, Some([4.0, 2.0]));
        let DrawCommand::Rect { stroke, stroke_width, corner_radius, shadow, .. } =
            &owned.commands[1]
        else {
            panic!("第二条应是矩形");
        };
        assert_eq!(*stroke, Some(PaintColor::Token { name: ColorToken::Border }));
        assert_eq!(*stroke_width, Some(1.0), "未填描边宽度按 1 处理");
        assert_eq!(*corner_radius, Some(2.0));
        assert_eq!(
            *shadow,
            Some(PaintShadow {
                offset_x: 1.0,
                offset_y: 2.0,
                blur: 3.0,
                color: PaintColor::Rgba { rgba: [0, 0, 0, 40] },
            })
        );
        let DrawCommand::Circle { cx, cy, r, fill, .. } = &owned.commands[2] else {
            panic!("第三条应是圆");
        };
        assert_eq!((*cx, *cy, *r), (10.0, 11.0, 12.0));
        assert_eq!(*fill, Some(PaintColor::Token { name: ColorToken::Accent }));
        assert_eq!(owned.ime, Some(FrameRect { x: 4.0, y: 5.0, w: 6.0, h: 18.0 }));
        assert_eq!(owned.fps, None, "0 帧拍不是有效节拍");
        assert_eq!(owned.size, None);
        assert!(owned.popup.is_none());
        for (code, token) in [
            (abi::TOKEN_BACKGROUND, ColorToken::Background),
            (abi::TOKEN_BORDER_SUBTLE, ColorToken::BorderSubtle),
            (abi::TOKEN_SELECTION, ColorToken::Selection),
            (abi::TOKEN_SCRIM, ColorToken::Scrim),
        ] {
            assert_eq!(paint_color(abi::AbiColor::token(code)), PaintColor::Token { name: token });
        }
    }

    #[test]
    fn frame_copy_rejects_unknown_commands_and_image_sources() {
        for (command, needle) in [
            (base(9999), "未知绘制指令"),
            (base(abi::CMD_IMAGE), "未知图片来源"),
        ] {
            let frame = frame_with(&command, 1);
            let error = unsafe { frame_from_abi(&frame) }.unwrap_err();
            assert!(error.contains(needle), "{error}");
        }
    }
}
