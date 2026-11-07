//! 灯箱（图片全屏预览）：浮层帧绘制与输入（滚轮缩放、拖动平移、双击复位、左右翻页、点外关闭）。

use atelyx_plugin_protocol::frame::{FrameRect, PopupCapture, PopupLayer, SurfacePopup};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, ImageFit, PaintColor};

use crate::images;
use crate::model::{Lightbox, Point, Rect, Session, Size};

/// 灯箱要盖住面板顶部头部行（宿主头部行高 30）。
pub const HEADER_INSET: f32 = 30.0;
/// 显示区内边距（px）。
const INSET: f32 = 48.0;
/// 上限图边长：与图片格缩略图同档（宿主对同一张图共用已解码句柄，点开即出），
/// 原图由宿主后台解码就绪后盖上（渐进升级）；原图期间放大到高倍数会短暂偏糊。
const PREVIEW_MAX_EDGE: u32 = crate::model::THUMB_MAX_EDGE;
/// 未声明排版口径时帧文本的相对行高（与宿主缺省一致）。
const TEXT_LINE: f32 = 1.2;

/// 灯箱当前图（仓库相对路径；下标越界时取末张）。
pub fn current_image(session: &Session, lightbox: &Lightbox) -> Option<String> {
    let row = session.data.rows.iter().find(|row| row.id == lightbox.row_id)?;
    let value = row.values.get(&lightbox.field_id)?;
    let list = images::entries(Some(value));
    list.get(lightbox.index.min(list.len().checked_sub(1)?)).cloned()
}

/// 当前图片格的图片张数。
pub fn image_count(session: &Session, lightbox: &Lightbox) -> usize {
    session
        .data
        .rows
        .iter()
        .find(|row| row.id == lightbox.row_id)
        .and_then(|row| row.values.get(&lightbox.field_id))
        .map(|value| images::entries(Some(value)).len())
        .unwrap_or(0)
}

/// 翻页钮半径与圆心距边（面本地坐标；绘制与命中同口径）。
pub const NAV_RADIUS: f32 = 16.0;
const NAV_INSET: f32 = 24.0;

/// 翻页钮矩形：`dir` = -1 左侧 / 1 右侧，垂直居中。
pub fn nav_button(size: Size, dir: i32) -> Rect {
    let cx = if dir < 0 {
        NAV_INSET
    } else {
        (size.width - NAV_INSET).max(NAV_INSET)
    };
    Rect::new(
        cx - NAV_RADIUS,
        size.height / 2.0 - NAV_RADIUS,
        NAV_RADIUS * 2.0,
        NAV_RADIUS * 2.0,
    )
}

/// 灯箱浮层帧：遮罩 + 图片（含平移）+ 翻页按钮 + 计数。
pub fn popup_frame(session: &Session, size: Size) -> Option<SurfacePopup> {
    let lightbox = session.view.lightbox.as_ref()?;
    let mut commands = Vec::new();
    let rect = FrameRect {
        x: 0.0,
        y: -HEADER_INSET,
        w: size.width.max(1.0),
        h: (size.height + HEADER_INSET).max(1.0),
    };
    // 遮罩
    commands.push(DrawCommand::Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        fill: Some(PaintColor::Token { name: ColorToken::Scrim }),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
    let inner = Rect::new(
        INSET,
        INSET - HEADER_INSET,
        (size.width - 2.0 * INSET).max(1.0),
        (size.height - 2.0 * INSET).max(1.0),
    );
    let display = display_rect(inner, lightbox.zoom, lightbox.pan);
    let count = image_count(session, lightbox);
    let mut hits = Vec::new();
    if let Some(error) = &lightbox.error {
        commands.push(centered_note(size, error, ColorToken::Danger));
    } else {
        match current_image(session, lightbox) {
            Some(rel) => {
                // 图片指令不等探针：协作空间下探针要整文件下载，等它收起占位会让
                // 首帧图片白白串行在下裁之后。加载占位文字垫在图片指令下方，未载入的
                // 句柄宿主直接跳过该条指令（文字露出），句柄一到即被盖住；上限图与
                // 图片格缩略图同档共用宿主句柄，点开即出，原图就绪后盖上
                if lightbox.loading {
                    commands.push(centered_note(size, "图片加载中…", ColorToken::TextMuted));
                }
                commands.push(DrawCommand::Image {
                    source: atelyx_plugin_protocol::paint::ImageSource::Vault {
                        path: rel.clone(),
                        max_edge: Some(PREVIEW_MAX_EDGE),
                    },
                    x: display.x,
                    y: display.y,
                    w: display.w,
                    h: display.h,
                    fit: Some(ImageFit::Contain),
                });
                // 原图层等探针回执后再发：探针已把整文件字节带入宿主缓存，此刻取数
                // 不再重复下载（协作空间无区间读接口，原图提前发会与探针各下一次整文件）
                if !lightbox.loading {
                    commands.push(DrawCommand::Image {
                        source: atelyx_plugin_protocol::paint::ImageSource::Vault {
                            path: rel,
                            max_edge: None,
                        },
                        x: display.x,
                        y: display.y,
                        w: display.w,
                        h: display.h,
                        fit: Some(ImageFit::Contain),
                    });
                }
            }
            None => commands.push(centered_note(size, "图片已清空", ColorToken::TextMuted)),
        }
    }
    // 翻页按钮（多图）：左右两侧垂直居中
    if count > 1 {
        for (dir, id) in [(-1, "lb:prev"), (1, "lb:next")] {
            let button = nav_button(size, dir);
            let cx = button.x + NAV_RADIUS;
            let cy = button.y + NAV_RADIUS;
            commands.push(DrawCommand::Circle {
                cx,
                cy,
                r: NAV_RADIUS,
                fill: Some(PaintColor::Token { name: ColorToken::Scrim }),
                stroke: None,
                stroke_width: None,
            });
            let tip = if dir < 0 { cx + 3.0 } else { cx - 3.0 };
            let back = if dir < 0 { cx - 3.0 } else { cx + 3.0 };
            commands.push(DrawCommand::Path {
                points: vec![
                    atelyx_plugin_protocol::paint::PaintPoint { x: tip, y: cy - 5.0 },
                    atelyx_plugin_protocol::paint::PaintPoint { x: back, y: cy },
                    atelyx_plugin_protocol::paint::PaintPoint { x: tip, y: cy + 5.0 },
                ],
                closed: false,
                fill: None,
                stroke: Some(PaintColor::Token { name: ColorToken::Text }),
                stroke_width: Some(1.5),
            });
            hits.push(atelyx_plugin_protocol::frame::HitRegion {
                id: id.to_string(),
                x: button.x,
                y: button.y,
                w: button.w,
                h: button.h,
                cursor: Some(atelyx_plugin_protocol::frame::HitCursor::Pointer),
            });
        }
    }
    // 计数（底部居中）
    if count > 1 {
        let index = lightbox.index.min(count - 1);
        commands.push(DrawCommand::Text {
            x: rect.x,
            y: rect.y + rect.h - 8.0 - crate::theme::font_caption() * TEXT_LINE,
            content: format!("{}/{}", index + 1, count),
            size: crate::theme::font_caption(),
            color: PaintColor::Token { name: ColorToken::Text },
            weight: None,
            family: None,
            family_name: None,
            italic: false,
            max_width: Some(rect.w),
            align: Some(atelyx_plugin_protocol::paint::TextAlign::Center),
            layout: None,
        });
    }
    Some(SurfacePopup {
        rect,
        capture: PopupCapture::All,
        layer: PopupLayer::Panel,
        order: 0,
        commands,
        hits,
        ime: None,
    })
}

/// 图片实际绘制矩形：Contain 适配居中于 `display` 内（按探测到的原图比例）。
/// 未探测到尺寸（仍在载入 / 失败）退回整个显示框——点外关闭退化为按显示框判定。
pub fn image_rect(display: Rect, source: (u32, u32)) -> Rect {
    if source.0 == 0 || source.1 == 0 {
        return display;
    }
    let scale = (display.w / source.0 as f32).min(display.h / source.1 as f32).max(0.0);
    let w = source.0 as f32 * scale;
    let h = source.1 as f32 * scale;
    Rect::new(
        display.x + (display.w - w) / 2.0,
        display.y + (display.h - h) / 2.0,
        w,
        h,
    )
}

/// 图片显示区内水平居中、垂直居中的提示文字（与图片同区，不压到头部行）。
fn centered_note(size: Size, content: &str, color: ColorToken) -> DrawCommand {
    let font = crate::theme::font_caption();
    DrawCommand::Text {
        x: 0.0,
        y: size.height / 2.0 - HEADER_INSET - font * TEXT_LINE / 2.0,
        content: content.to_string(),
        size: font,
        color: PaintColor::Token { name: color },
        weight: None,
        family: None,
        family_name: None,
        italic: false,
        max_width: Some(size.width.max(1.0)),
        align: Some(atelyx_plugin_protocol::paint::TextAlign::Center),
        layout: None,
    }
}

/// 灯箱显示区：内框按倍率放大并居中，叠加夹紧后的平移；图片由宿主在框内按比例 `Contain`。
pub fn display_rect(inner: Rect, zoom: f32, pan: Point) -> Rect {
    let w = inner.w * zoom;
    let h = inner.h * zoom;
    let base_x = inner.x - (w - inner.w) / 2.0;
    let base_y = inner.y - (h - inner.h) / 2.0;
    let max_pan_x = ((w - inner.w) / 2.0).max(0.0);
    let max_pan_y = ((h - inner.h) / 2.0).max(0.0);
    Rect::new(
        base_x + pan.x.clamp(-max_pan_x, max_pan_x),
        base_y + pan.y.clamp(-max_pan_y, max_pan_y),
        w,
        h,
    )
}

/// 把累积平移夹回可拖范围，避免松手后状态越界、下一次拖拽先跳一段。
pub fn clamp_pan(lightbox: &mut Lightbox, size: Size) {
    let inner = inner_rect(size);
    let max_x = ((inner.w * lightbox.zoom - inner.w) / 2.0).max(0.0);
    let max_y = ((inner.h * lightbox.zoom - inner.h) / 2.0).max(0.0);
    lightbox.pan = Point::new(
        lightbox.pan.x.clamp(-max_x, max_x),
        lightbox.pan.y.clamp(-max_y, max_y),
    );
}

/// 灯箱显示区（面本地坐标，供输入判定复用）。
pub fn inner_rect(size: Size) -> Rect {
    Rect::new(
        INSET,
        INSET - HEADER_INSET,
        (size.width - 2.0 * INSET).max(1.0),
        (size.height - 2.0 * INSET).max(1.0),
    )
}

/// 翻页：`dir` = -1 上一张 / 1 下一张（取模成环，切图复位缩放）。
pub fn navigate(lightbox: &mut Lightbox, count: usize, dir: i32) {
    if count == 0 {
        return;
    }
    lightbox.index = (lightbox.index as i32 + dir).rem_euclid(count as i32) as usize;
    lightbox.reset_zoom();
    lightbox.loading = true;
    lightbox.error = None;
    // 上一张的尺寸不再是这一张的：未探测前点外关闭退回按显示框判定
    lightbox.source = (0, 0);
}

/// 滚轮缩放（以光标为锚）：协议里 `delta_y` 为正 = 向下滚 = 缩小。
pub fn zoom_at(lightbox: &mut Lightbox, size: Size, cursor: Point, delta_y: f32) {
    let before = display_rect(inner_rect(size), lightbox.zoom, lightbox.pan);
    let factor = (-delta_y * 0.0035).exp();
    let next = (lightbox.zoom * factor).clamp(1.0, 8.0);
    if (next - lightbox.zoom).abs() < f32::EPSILON {
        return;
    }
    // 以光标为锚：缩放后同一点仍落在光标处
    let anchor_x = (cursor.x - before.x) / before.w.max(1.0);
    let anchor_y = (cursor.y - before.y) / before.h.max(1.0);
    lightbox.zoom = next;
    let after = display_rect(inner_rect(size), lightbox.zoom, lightbox.pan);
    let target_x = cursor.x - anchor_x * after.w;
    let target_y = cursor.y - anchor_y * after.h;
    lightbox.pan = Point::new(target_x - after.x + lightbox.pan.x, target_y - after.y + lightbox.pan.y);
    clamp_pan(lightbox, size);
}

#[cfg(test)]
mod tests {
    use atelyx_format::table::{TableField, TableFile, TableRow};
    use atelyx_plugin_protocol::paint::{DrawCommand, ImageSource};

    use super::{nav_button, navigate, popup_frame};
    use crate::model::{Lightbox, Session, Size};

    fn session() -> Session {
        let mut values = serde_json::Map::new();
        values.insert("f".into(), serde_json::json!(["a.png", "b.png"]));
        let mut session = Session::new(
            "t.atb".into(),
            TableFile {
                schema: "atelyx-table/v1".into(),
                id: "t".into(),
                title: "T".into(),
                fields: vec![TableField {
                    id: "f".into(),
                    name: "A".into(),
                    field_type: "image".into(),
                    options: None,
                    width: None,
                    calc_type: None,
                }],
                rows: vec![TableRow { id: "r".into(), values, height: None, styles: None }],
                created_at: 0,
                updated_at: 0,
            },
        );
        session.view.canvas_size = Size { width: 400.0, height: 300.0 };
        session.view.lightbox = Some(Lightbox::new("r".into(), "f".into(), 0));
        session
    }

    #[test]
    fn navigate_wraps_and_resets_loading() {
        let mut lightbox = Lightbox::new("row".into(), "field".into(), 0);
        lightbox.loading = false;
        lightbox.zoom = 2.0;
        lightbox.error = Some("旧错误".into());
        navigate(&mut lightbox, 3, -1);
        assert_eq!(lightbox.index, 2);
        assert_eq!(lightbox.zoom, 1.0);
        assert!(lightbox.loading);
        assert_eq!(lightbox.error, None);
        navigate(&mut lightbox, 3, 1);
        assert_eq!(lightbox.index, 0);
    }

    #[test]
    fn nav_buttons_stay_inside_narrow_panel() {
        let size = Size { width: 120.0, height: 200.0 };
        let left = nav_button(size, -1);
        let right = nav_button(size, 1);
        assert_eq!(left.x, 8.0);
        assert_eq!(right.x + right.w, size.width - 8.0);
        assert!(nav_button(Size { width: 40.0, height: 200.0 }, 1).x >= 0.0);
    }

    #[test]
    fn loading_keeps_the_grid_tier_and_the_original_tier_waits_for_the_probe() {
        let session = session();
        let popup = popup_frame(&session, session.view.canvas_size).expect("应有浮层");
        // 加载中即发上限图（与图片格缩略图同档、共用宿主句柄），只此一层图片指令
        let sources: Vec<_> = popup
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Image { source, .. } => Some(source.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(sources.len(), 1);
        assert!(matches!(
            &sources[0],
            ImageSource::Vault { max_edge: Some(edge), .. } if *edge == crate::model::THUMB_MAX_EDGE
        ));
        assert!(popup.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { content, .. } if content == "图片加载中…"
        )));
        // 探针回执后补发原图层（共两层）
        let mut session = session;
        if let Some(lightbox) = session.view.lightbox.as_mut() {
            lightbox.loading = false;
        }
        let popup = popup_frame(&session, session.view.canvas_size).expect("应有浮层");
        assert_eq!(
            popup
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::Image { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn error_shows_note_instead_of_image() {
        let mut session = session();
        if let Some(lightbox) = session.view.lightbox.as_mut() {
            lightbox.loading = false;
            lightbox.error = Some("无法加载图片".into());
        }
        let popup = popup_frame(&session, session.view.canvas_size).expect("应有浮层");
        assert!(!popup.commands.iter().any(|command| matches!(command, DrawCommand::Image { .. })));
        assert!(popup.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { content, .. } if content == "无法加载图片"
        )));
    }

    #[test]
    fn loaded_image_emits_image_command() {
        let mut session = session();
        if let Some(lightbox) = session.view.lightbox.as_mut() {
            lightbox.loading = false;
        }
        let popup = popup_frame(&session, session.view.canvas_size).expect("应有浮层");
        assert!(popup.commands.iter().any(|command| matches!(command, DrawCommand::Image { .. })));
    }

    /// Contain 留白按原图比例收缩：竖图在宽框里两侧留白不算图内，点它应能退出。
    #[test]
    fn image_rect_shrinks_to_contain_fit() {
        let display = super::Rect::new(0.0, 0.0, 400.0, 100.0);
        let rect = super::image_rect(display, (100, 100));
        assert_eq!(rect, super::Rect::new(150.0, 0.0, 100.0, 100.0));
        assert!(!rect.contains(50.0, 50.0), "左留白不在图内");
        assert!(rect.contains(200.0, 50.0));
        // 未探测到尺寸退回整个显示框
        assert_eq!(super::image_rect(display, (0, 0)), display);
    }
}
