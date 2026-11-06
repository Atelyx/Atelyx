//! 网格几何：列宽 / 行高 / 滚动范围 / 命中判定。坐标一律面本地逻辑像素，尺寸先乘缩放。

use crate::model::{Hit, LayoutCache, Rect, Size, ADD_COL_W, GUTTER_W, HEADER_H, NEW_ROW_H, STATUS_H};

/// 网格几何快照（一次渲染内固定）。
pub struct Geo<'a> {
    pub cache: &'a LayoutCache,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub width: f32,
    pub height: f32,
    pub z: f32,
}

impl<'a> Geo<'a> {
    /// 表头带高（已缩放）。
    pub fn header_h(&self) -> f32 {
        HEADER_H * self.z
    }

    /// 行号列宽（已缩放）。
    pub fn gutter_w(&self) -> f32 {
        GUTTER_W * self.z
    }

    /// 加列占位宽（已缩放）。
    pub fn add_col_w(&self) -> f32 {
        ADD_COL_W * self.z
    }

    /// 新行条高（已缩放）。
    pub fn new_row_h(&self) -> f32 {
        NEW_ROW_H * self.z
    }

    /// 状态栏高（已缩放）。
    pub fn status_h(&self) -> f32 {
        STATUS_H * self.z
    }

    /// 滚动条占位宽（与宿主同口径的细条）。
    pub fn scrollbar_w(&self) -> f32 {
        8.0
    }

    pub fn col_w(&self, index: usize) -> f32 {
        self.cache.col_w.get(index).copied().unwrap_or(0.0)
    }

    pub fn row_h(&self, index: usize) -> f32 {
        self.cache.row_h.get(index).copied().unwrap_or(0.0)
    }

    pub fn col_x(&self, index: usize) -> f32 {
        self.cache.col_x.get(index).copied().unwrap_or(0.0)
    }

    pub fn row_y(&self, index: usize) -> f32 {
        self.cache.row_y.get(index).copied().unwrap_or(0.0)
    }

    /// 内容区左缘（行号列右缘）。
    pub fn grid_left(&self) -> f32 {
        self.gutter_w()
    }

    /// 内容区上界（表头下缘）。
    pub fn grid_top(&self) -> f32 {
        self.header_h()
    }

    /// 列区可用宽。
    pub fn viewport_w(&self) -> f32 {
        (self.width - self.gutter_w() - self.scrollbar_w()).max(1.0)
    }

    /// 行区可用高（扣表头与状态栏）。
    pub fn viewport_h(&self) -> f32 {
        (self.height - self.header_h() - self.status_h()).max(1.0)
    }

    /// 可视行区间（含两端）。
    pub fn visible_rows(&self, count: usize) -> (usize, usize) {
        if count == 0 {
            return (0, 0);
        }
        let scroll = self.scroll_y;
        let bottom = scroll + self.viewport_h();
        let mut first = 0;
        while first + 1 < count && self.row_y(first + 1) <= scroll {
            first += 1;
        }
        let mut last = first;
        while last + 1 < count && self.row_y(last + 1) < bottom {
            last += 1;
        }
        (first, last)
    }

    /// 列在视口内的判断。
    pub fn col_visible(&self, index: usize) -> bool {
        let left = self.col_x(index) - self.scroll_x;
        let right = left + self.col_w(index);
        right > 0.0 && left < self.viewport_w()
    }

    pub fn content_h(&self) -> f32 {
        self.cache.content_h
    }

    pub fn total_w(&self) -> f32 {
        self.cache.total_w
    }

    pub fn max_scroll_x(&self) -> f32 {
        (self.total_w() - self.viewport_w()).max(0.0)
    }

    pub fn max_scroll_y(&self) -> f32 {
        (self.content_h() - self.viewport_h()).max(0.0)
    }

    /// 竖向滚动条滑块：`(起点, 长度, 行程)`（起点相对轨道顶 = 表头下缘）。
    pub fn v_thumb(&self) -> (f32, f32, f32) {
        let track = (self.height - self.status_h() - self.header_h()).max(0.0);
        let len = (track * (self.viewport_h() / self.content_h().max(1.0))).max(24.0).min(track);
        let travel = (track - len).max(0.0);
        let start = if self.max_scroll_y() > 0.0 {
            (self.scroll_y / self.max_scroll_y() * travel).clamp(0.0, travel)
        } else {
            0.0
        };
        (start, len, travel)
    }

    /// 横向滚动条滑块：`(起点, 长度, 行程)`（轨道左缘 = 内容区左缘）。
    pub fn h_thumb(&self) -> (f32, f32, f32) {
        let track = self.width - self.gutter_w() - self.scrollbar_w();
        let len = (track * (self.viewport_w() / self.total_w().max(1.0))).max(24.0).min(track);
        let travel = (track - len).max(0.0);
        let start = if self.max_scroll_x() > 0.0 {
            (self.scroll_x / self.max_scroll_x() * travel).clamp(0.0, travel)
        } else {
            0.0
        };
        (start, len, travel)
    }

    /// 单元格矩形（面本地坐标）。
    pub fn cell_rect(&self, row: usize, field: usize) -> Rect {
        let x = self.grid_left() + self.col_x(field) - self.scroll_x;
        let y = self.grid_top() + self.row_y(row) - self.scroll_y;
        Rect::new(x, y, self.col_w(field), self.row_h(row))
    }

    fn header_hit(&self, x: f32, y: f32) -> Option<Hit> {
        if y >= self.header_h() {
            return None;
        }
        let Some(field) = self.field_at_x(x) else {
            return Some(Hit::AddFieldCol);
        };
        let left = self.grid_left() + self.col_x(field) - self.scroll_x;
        let width = self.col_w(field);
        if x >= left + width - 8.0 * self.z {
            return Some(Hit::HeaderResize { field });
        }
        if x >= left + width - 18.0 * self.z {
            return Some(Hit::HeaderMenu { field });
        }
        Some(Hit::Header { field })
    }

    /// 命中判定：按面本地坐标给出语义分类。
    pub fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        if x < 0.0 || y < 0.0 || x > self.width || y > self.height {
            return None;
        }
        if y >= self.height - self.status_h() {
            let field = self.field_at_x(x)?;
            return Some(Hit::Status { field });
        }
        if y < self.header_h() {
            if x < self.gutter_w() {
                return Some(Hit::Corner);
            }
            return self.header_hit(x, y);
        }
        if self.max_scroll_x() > 0.0
            && y >= self.height - self.status_h() - self.scrollbar_w()
            && x >= self.gutter_w()
        {
            return Some(Hit::HScrollbar);
        }
        if self.max_scroll_y() > 0.0 && x >= self.width - self.scrollbar_w() {
            return Some(Hit::VScrollbar);
        }
        if x < self.gutter_w() {
            let row = self.row_at_y(y)?;
            let top = self.grid_top() + self.row_y(row) - self.scroll_y;
            let height = self.row_h(row);
            if y >= top + height - 6.0 * self.z {
                return Some(Hit::GutterHandle { row });
            }
            if (y - (top + height / 2.0)).abs() <= 9.0 * self.z {
                return Some(Hit::GutterGrip { row });
            }
            return Some(Hit::Gutter { row });
        }
        if y < self.header_h() {
            return self.header_hit(x, y);
        }
        let add_left =
            self.grid_left() + self.cache.col_x.last().copied().unwrap_or(0.0) - self.scroll_x;
        if x >= add_left && y >= self.grid_top() {
            return Some(Hit::AddFieldCol);
        }
        let Some(row) = self.row_at_y(y) else {
            let last_bottom =
                self.grid_top() + self.row_y(self.cache.row_h.len()) - self.scroll_y;
            return (y >= last_bottom && x >= self.gutter_w()).then_some(Hit::NewRow);
        };
        let field = self.field_at_x(x)?;
        Some(Hit::Cell { row, field })
    }

    /// 横坐标 → 列下标（已减滚动）。
    pub fn field_at_x(&self, x: f32) -> Option<usize> {
        let local = x - self.grid_left() + self.scroll_x;
        if local < 0.0 || local > self.cache.total_w {
            return None;
        }
        let index = self.cache.col_x.iter().rposition(|left| *left <= local)?;
        (index < self.cache.col_w.len()).then_some(index)
    }

    /// 纵坐标 → 行下标（已减滚动）。
    pub fn row_at_y(&self, y: f32) -> Option<usize> {
        let local = y - self.grid_top() + self.scroll_y;
        if local < 0.0 || local > self.cache.row_y.last().copied().unwrap_or(0.0) {
            return None;
        }
        self.cache.row_y.iter().rposition(|top| *top <= local)
    }
}

/// 网格几何入口：布局缓存 + 当前滚动 / 缩放 + 面尺寸（只借布局缓存，便于与视图态字段分开借用）。
pub fn geo<'a>(
    layout: &'a Option<LayoutCache>,
    scroll_x: f32,
    scroll_y: f32,
    zoom: f32,
    size: Size,
) -> Option<Geo<'a>> {
    let cache = layout.as_ref()?;
    Some(Geo { cache, scroll_x, scroll_y, width: size.width, height: size.height, z: zoom })
}

/// 把滚动位收回当前几何的合法区间（布局/缩放变化后调用）。
pub fn clamp_scroll(session: &mut crate::model::Session) {
    let Some(geo) = geo(
        &session.view.layout,
        session.view.scroll_x,
        session.view.scroll_y,
        session.view.zoom,
        session.view.canvas_size,
    ) else {
        return;
    };
    let (max_x, max_y) = (geo.max_scroll_x(), geo.max_scroll_y());
    session.view.scroll_x = session.view.scroll_x.clamp(0.0, max_x);
    session.view.scroll_y = session.view.scroll_y.clamp(0.0, max_y);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LayoutCache;

    fn cache(content_h: f32) -> LayoutCache {
        LayoutCache {
            col_w: vec![120.0],
            col_x: vec![0.0, 120.0],
            total_w: 120.0,
            row_h: vec![content_h],
            row_y: vec![0.0, content_h],
            content_h,
            wrap: Default::default(),
        }
    }

    fn geo<'a>(cache: &'a LayoutCache) -> Geo<'a> {
        Geo {
            cache,
            scroll_x: 0.0,
            scroll_y: 0.0,
            width: 300.0,
            height: 200.0,
            z: 1.0,
        }
    }

    #[test]
    fn scroll_range_excludes_header_and_status() {
        let cache = cache(400.0);
        let geo = geo(&cache);
        // 可视高 = 200 - 32(表头) - 32(状态栏)；内容高只和这部分比
        assert_eq!(geo.max_scroll_y(), 400.0 - 136.0);
    }

    #[test]
    fn vertical_thumb_tracks_between_header_and_status() {
        let cache = cache(400.0);
        let mut geo = geo(&cache);
        geo.scroll_y = geo.max_scroll_y();
        let (start, len, travel) = geo.v_thumb();
        assert!(start <= travel);
        assert!((start + len - (geo.height - geo.status_h() - geo.header_h())).abs() < 0.01);
    }

    #[test]
    fn status_row_wins_over_scrollbar_hit() {
        let cache = cache(400.0);
        let geo = geo(&cache);
        // 状态栏同时压在竖条区域：状态栏优先
        assert!(matches!(
            geo.hit_at(100.0, 199.0),
            Some(Hit::Status { .. })
        ));
    }
}
