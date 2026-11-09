//! Markdown 表格自绘：列宽按内容自适应、单元格换行、行列分隔线单线绘制。

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Renderer as _, Shell};
use iced::{Background, Color, Element, Length, Point, Rectangle, Size, Theme, Vector};

use crate::Message;

/// 列宽下限（含单元格左右内边距）；可用宽不足时按列数均分。
pub(crate) const MIN_COL_W: f32 = 40.0;
/// 单列自然宽上限（占可用宽比例）：避免一列长文挤掉其余列。
const MAX_COL_FRAC: f32 = 0.6;

/// Markdown 表格部件：表头 + 数据行已展平成单元格元素（行优先，表头在最前）。
pub struct MarkdownTable<'a> {
    columns: usize,
    cells: Vec<Element<'a, Message>>,
    /// 各列自然宽（含单元格内边距）。
    natural: Vec<f32>,
    border: Color,
    header_bg: Color,
    separator: f32,
}

impl<'a> MarkdownTable<'a> {
    pub(crate) fn new(
        columns: usize,
        cells: Vec<Element<'a, Message>>,
        natural: Vec<f32>,
        border: Color,
        header_bg: Color,
        separator: f32,
    ) -> Self {
        let columns = columns.max(1);
        Self { columns, cells, natural, border, header_bg, separator }
    }

    /// 列宽：自然宽在可用宽内按比例铺满；超出时按比例压缩并保证下限。
    pub(crate) fn column_widths(&self, available: f32) -> Vec<f32> {
        column_widths(&self.natural, available, self.separator, self.columns)
    }
}

/// 列宽分配（纯函数，供部件与单测共用）。
///
/// 先按自然宽铺满剩余空间；总宽超出时按自然宽比例压缩，压到 MIN_COL_W 后
/// 再把仍然溢出的部分从高于下限的列按比例削掉。返回列宽之和恒等于可用宽（无可用宽时
/// 退回自然宽）。
pub(crate) fn column_widths(
    natural: &[f32],
    available: f32,
    separator: f32,
    columns: usize,
) -> Vec<f32> {
    let columns = columns.max(1);
    let mut natural: Vec<f32> = (0..columns)
        .map(|i| natural.get(i).copied().unwrap_or(0.0).max(0.0))
        .collect();
    let gaps = separator.max(0.0) * columns.saturating_sub(1) as f32;
    let usable = (available - gaps).max(0.0);
    if !available.is_finite() || usable <= 0.0 {
        return natural;
    }

    let min_w = MIN_COL_W.min(usable / columns as f32);
    // 单列过长时先压低自然宽，剩余空间再按比例分给其他列
    let cap = usable * MAX_COL_FRAC;
    for width in &mut natural {
        *width = width.min(cap);
    }
    let total: f32 = natural.iter().sum();
    let mut widths: Vec<f32> = if total <= f32::EPSILON {
        vec![usable / columns as f32; columns]
    } else {
        let floors: Vec<f32> = natural.iter().map(|width| width.max(min_w)).collect();
        let floor_total: f32 = floors.iter().sum();
        if floor_total <= usable {
            // 先保下限，剩余空间按自然宽比例分配（空列也有可点的最小宽度）
            let extra = usable - floor_total;
            natural
                .iter()
                .zip(&floors)
                .map(|(width, floor)| floor + extra * (*width / total))
                .collect()
        } else {
            let scale = usable / total.max(1.0);
            natural
                .iter()
                .map(|width| (width * scale).max(min_w))
                .collect()
        }
    };

    let mut sum: f32 = widths.iter().sum();
    if sum > usable {
        let adjustable: f32 = widths.iter().map(|width| (width - min_w).max(0.0)).sum();
        let overflow = sum - usable;
        if adjustable > 0.0 {
            let factor = ((adjustable - overflow) / adjustable).clamp(0.0, 1.0);
            for width in &mut widths {
                if *width > min_w {
                    *width = min_w + (*width - min_w) * factor;
                }
            }
        }
        sum = widths.iter().sum();
    }

    // 浮点余量归到最后一列，内容区始终恰好等于可用宽
    if let Some(last) = widths.last_mut() {
        *last = (*last + usable - sum).max(min_w.min(usable));
    }
    widths
}

impl Widget<Message, Theme, iced::Renderer> for MarkdownTable<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn children(&self) -> Vec<Tree> {
        self.cells.iter().map(|cell| Tree::new(cell.as_widget())).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.cells);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let columns = self.columns.max(1);
        let rows = self.cells.len().div_ceil(columns).max(1);
        let widths = self.column_widths(limits.max().width);
        let mut nodes: Vec<Option<layout::Node>> = Vec::with_capacity(self.cells.len());
        let mut heights = vec![0.0f32; rows];

        for row in 0..rows {
            for column in 0..columns {
                let index = row * columns + column;
                let Some(cell) = self.cells.get_mut(index) else {
                    nodes.push(None);
                    continue;
                };
                let width = widths.get(column).copied().unwrap_or(0.0);
                let cell_limits = layout::Limits::new(
                    Size::ZERO,
                    Size::new(width, f32::INFINITY),
                )
                .width(Length::Fixed(width));
                let node = cell
                    .as_widget_mut()
                    .layout(&mut tree.children[index], renderer, &cell_limits);
                heights[row] = heights[row].max(node.size().height);
                nodes.push(Some(node));
            }
        }

        let total_width: f32 = widths.iter().sum::<f32>()
            + self.separator.max(0.0) * columns.saturating_sub(1) as f32;
        let total_height: f32 = heights.iter().sum();
        let mut children = Vec::with_capacity(nodes.len());
        let mut y = 0.0;
        for row in 0..rows {
            let mut x = 0.0;
            for column in 0..columns {
                let index = row * columns + column;
                let Some(node) = nodes.get_mut(index).and_then(Option::take) else {
                    continue;
                };
                children.push(node.move_to(Point::new(x, y)));
                x += widths.get(column).copied().unwrap_or(0.0);
                if column + 1 < columns {
                    x += self.separator.max(0.0);
                }
            }
            y += heights[row];
        }
        layout::Node::with_children(Size::new(total_width, total_height), children)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((cell, state), child_layout) in
            self.cells.iter_mut().zip(&mut tree.children).zip(layout.children())
        {
            cell.as_widget_mut().update(
                state,
                event,
                child_layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let columns = self.columns.max(1);
        let mut column_x = Vec::new();
        let mut row_y = Vec::new();
        for (index, child) in layout.children().enumerate() {
            if index < columns {
                column_x.push(child.bounds().x);
            }
            if index % columns == 0 {
                row_y.push(child.bounds().y);
            }
        }
        // 表头整行底色（单元格高度不齐时也铺满）
        let header_bottom = row_y.get(1).copied().unwrap_or(bounds.y + bounds.height);
        renderer.fill_quad(
            Quad {
                bounds: Rectangle::new(
                    Point::new(bounds.x, bounds.y),
                    Size::new(bounds.width, header_bottom - bounds.y),
                ),
                ..Quad::default()
            },
            Background::Color(self.header_bg),
        );

        for ((cell, state), child_layout) in
            self.cells.iter().zip(&tree.children).zip(layout.children())
        {
            cell.as_widget().draw(state, renderer, theme, style, child_layout, cursor, viewport);
        }

        // 分隔线按实际单元格边界绘制（表头背景 / 行高差都不会把线画到格中）
        let separator = self.separator.max(0.0);
        if separator <= 0.0 {
            return;
        }
        for x in column_x.iter().skip(1) {
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle::new(Point::new(*x, bounds.y), Size::new(separator, bounds.height)),
                    ..Quad::default()
                },
                Background::Color(self.border),
            );
        }
        for y in row_y.iter().skip(1) {
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle::new(Point::new(bounds.x, *y), Size::new(bounds.width, separator)),
                    ..Quad::default()
                },
                Background::Color(self.border),
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        for ((cell, state), child_layout) in
            self.cells.iter_mut().zip(&mut tree.children).zip(layout.children())
        {
            cell.as_widget_mut().operate(state, child_layout, renderer, operation);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if !layout.bounds().intersects(viewport) {
            return mouse::Interaction::None;
        }
        self.cells
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((cell, state), child_layout)| {
                cell.as_widget()
                    .mouse_interaction(state, child_layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        overlay::from_children(&mut self.cells, tree, layout, renderer, viewport, translation)
    }
}

impl<'a> From<MarkdownTable<'a>> for Element<'a, Message> {
    fn from(table: MarkdownTable<'a>) -> Self {
        Element::new(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sum(widths: &[f32], available: f32, separator: f32) {
        let sum: f32 = widths.iter().sum::<f32>() + separator * widths.len().saturating_sub(1) as f32;
        assert!((sum - available).abs() < 0.5, "列宽总和不等于可用宽：{widths:?}");
    }

    #[test]
    fn content_widths_fill_available_space() {
        let widths = column_widths(&[80.0, 240.0], 600.0, 1.0, 2);
        assert!(widths[1] > widths[0]);
        assert_sum(&widths, 600.0, 1.0);
    }

    #[test]
    fn overflow_is_compressed_to_fit() {
        let widths = column_widths(&[400.0, 400.0, 400.0], 300.0, 1.0, 3);
        assert_sum(&widths, 300.0, 1.0);
        assert!(widths.iter().all(|width| *width >= MIN_COL_W.min(98.0)));
    }

    #[test]
    fn single_column_uses_full_width() {
        let widths = column_widths(&[12.0], 500.0, 1.0, 1);
        assert!((widths[0] - 500.0).abs() < 0.5);
    }

    #[test]
    fn empty_column_keeps_minimum_next_to_content() {
        let widths = column_widths(&[100.0, 0.0], 500.0, 1.0, 2);
        assert!(widths[1] >= MIN_COL_W);
        assert_sum(&widths, 500.0, 1.0);
    }

    #[test]
    fn empty_columns_share_available_width() {
        let widths = column_widths(&[0.0, 0.0, 0.0], 600.0, 1.0, 3);
        assert!(widths.iter().all(|width| (*width - 199.33).abs() < 1.0));
        assert_sum(&widths, 600.0, 1.0);
    }

    #[test]
    fn long_column_keeps_short_column_above_minimum() {
        let widths = column_widths(&[2000.0, 40.0], 600.0, 1.0, 2);
        assert!(widths[0] < 600.0);
        assert!(widths[1] > 40.0);
        assert_sum(&widths, 600.0, 1.0);
    }
}
