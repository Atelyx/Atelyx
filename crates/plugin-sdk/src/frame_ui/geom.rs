//! 帧内几何：面本地逻辑像素矩形与裁剪求交。

/// 面本地矩形（左上原点）。自绘闭包用它取控件区域并登记子区命中。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    /// 是否整体落在 `outer` 内。求交会引入末位误差，这里按 0.01 的容差判，
    /// 直接比较求交结果与自身相等会把本就在视口内的矩形误判成越界。
    pub fn covered_by(&self, outer: Rect) -> bool {
        const EPSILON: f32 = 0.01;
        self.x >= outer.x - EPSILON
            && self.y >= outer.y - EPSILON
            && self.right() <= outer.right() + EPSILON
            && self.bottom() <= outer.bottom() + EPSILON
    }

    /// 交集；不相交时返回空矩形（宽或高为 0）。
    pub fn intersect(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
    }
}
