//! SVG path `d` 数据解析与折线展平：图标描边绘制的数据通道。
//! 支持 M/L/H/V/C/S/Q/T/A/Z（含小写相对形式与隐式重复）；
//! 圆弧按端点参数化转三次贝塞尔，贝塞尔按固定段数展平为折线。

/// 单条子路径的展平产物：顶点折线（首点 = 起点）。
pub type Polyline = Vec<[f32; 2]>;

/// 一个图元展平后的子路径集合。
pub type FlatShapes = Vec<Polyline>;

/// 解析并展平 path `d`。遇到未知命令即停止（图标数据为静态可信来源）。
pub fn flatten_path(d: &str) -> FlatShapes {
    let mut t = Tokenizer::new(d);
    let mut out: FlatShapes = Vec::new();
    let mut cur: Option<Polyline> = None;
    let mut pen = [0.0f32, 0.0];
    let mut start = [0.0f32, 0.0];
    // S/T 的控制点反射依赖上一命令
    let mut prev_cubic_ctrl: Option<[f32; 2]> = None;
    let mut prev_quad_ctrl: Option<[f32; 2]> = None;
    let mut last_cmd: u8 = 0;

    while let Some((raw, relative)) = t.command() {
        let cmd = raw.to_ascii_uppercase() as u8;
        // 每条命令之后跟数字 = 隐式重复同一命令（L/H/V/C/S/Q/T/A 均适用）
        t.set_repeat(raw);
        // 数据未以 M 开头时，绘制命令以当前笔位开子路径
        if cmd != b'M' && cur.is_none() {
            cur = Some(vec![pen]);
        }
        match cmd {
            b'M' => {
                let Some(p) = t.point(relative, pen) else { break };
                if let Some(c) = cur.take() {
                    if c.len() > 1 {
                        out.push(c);
                    }
                }
                cur = Some(vec![p]);
                pen = p;
                start = p;
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
            b'L' => {
                let Some(p) = t.point(relative, pen) else { break };
                if let Some(c) = &mut cur {
                    c.push(p);
                }
                pen = p;
            }
            b'H' => {
                let Some(x) = t.number() else { break };
                let x = if relative { pen[0] + x } else { x };
                if let Some(c) = &mut cur {
                    c.push([x, pen[1]]);
                }
                pen = [x, pen[1]];
            }
            b'V' => {
                let Some(y) = t.number() else { break };
                let y = if relative { pen[1] + y } else { y };
                if let Some(c) = &mut cur {
                    c.push([pen[0], y]);
                }
                pen = [pen[0], y];
            }
            b'C' => {
                let Some((c1, c2, to)) = t.cubic(relative, pen) else { break };
                emit_cubic(&mut cur, pen, c1, c2, to);
                prev_cubic_ctrl = Some(c2);
                prev_quad_ctrl = None;
                pen = to;
            }
            b'S' => {
                let Some((c2, to)) = t.pair2(relative, pen) else { break };
                let c1 = match prev_cubic_ctrl {
                    Some(ctrl) if matches!(last_cmd, b'C' | b'S') => [
                        2.0 * pen[0] - ctrl[0],
                        2.0 * pen[1] - ctrl[1],
                    ],
                    _ => pen,
                };
                emit_cubic(&mut cur, pen, c1, c2, to);
                prev_cubic_ctrl = Some(c2);
                prev_quad_ctrl = None;
                pen = to;
            }
            b'Q' => {
                let Some((ctrl, to)) = t.pair2(relative, pen) else { break };
                quad_as_cubic(&mut cur, pen, ctrl, to);
                prev_quad_ctrl = Some(ctrl);
                prev_cubic_ctrl = None;
                pen = to;
            }
            b'T' => {
                let Some(to) = t.point(relative, pen) else { break };
                let ctrl = match prev_quad_ctrl {
                    Some(ctrl) if matches!(last_cmd, b'Q' | b'T') => [
                        2.0 * pen[0] - ctrl[0],
                        2.0 * pen[1] - ctrl[1],
                    ],
                    _ => pen,
                };
                quad_as_cubic(&mut cur, pen, ctrl, to);
                prev_quad_ctrl = Some(ctrl);
                prev_cubic_ctrl = None;
                pen = to;
            }
            b'A' => {
                let Some((rx, ry, rot, large, sweep, to)) = t.arc(relative, pen) else { break };
                for (c1, c2, seg_to) in arc_to_cubics(pen, rx, ry, rot, large, sweep, to) {
                    emit_cubic(&mut cur, pen, c1, c2, seg_to);
                    pen = seg_to;
                }
                prev_cubic_ctrl = None;
                prev_quad_ctrl = None;
            }
            b'Z' => {
                if let Some(c) = &mut cur {
                    c.push(start);
                }
                pen = start;
            }
            _ => break,
        }
        last_cmd = cmd;
    }
    if let Some(c) = cur.take() {
        if c.len() > 1 {
            out.push(c);
        }
    }
    out
}

/// 二次曲线升三次并展平入当前子路径。
fn quad_as_cubic(cur: &mut Option<Polyline>, p0: [f32; 2], ctrl: [f32; 2], to: [f32; 2]) {
    let c1 = [
        p0[0] + 2.0 / 3.0 * (ctrl[0] - p0[0]),
        p0[1] + 2.0 / 3.0 * (ctrl[1] - p0[1]),
    ];
    let c2 = [
        to[0] + 2.0 / 3.0 * (ctrl[0] - to[0]),
        to[1] + 2.0 / 3.0 * (ctrl[1] - to[1]),
    ];
    emit_cubic(cur, p0, c1, c2, to);
}

fn emit_cubic(cur: &mut Option<Polyline>, p0: [f32; 2], c1: [f32; 2], c2: [f32; 2], to: [f32; 2]) {
    const STEPS: usize = 10;
    if let Some(c) = cur {
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            c.push(bezier_at(p0, c1, c2, to, t));
        }
    }
}

fn bezier_at(p0: [f32; 2], c1: [f32; 2], c2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let c = 3.0 * u * t * t;
    let d = t * t * t;
    [
        a * p0[0] + b * c1[0] + c * c2[0] + d * p3[0],
        a * p0[1] + b * c1[1] + c * c2[1] + d * p3[1],
    ]
}

/// 端点参数化圆弧 → ≤90° 的三次贝塞尔序列（标准中心参数化算法）。
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    from: [f32; 2],
    rx: f32,
    ry: f32,
    x_rotation_deg: f32,
    large_arc: bool,
    sweep: bool,
    to: [f32; 2],
) -> Vec<([f32; 2], [f32; 2], [f32; 2])> {
    let (rx, ry) = (rx.abs(), ry.abs());
    if rx < f32::EPSILON || ry < f32::EPSILON {
        return vec![(from, to, to)];
    }
    let phi = x_rotation_deg.to_radians();
    // sin_cos 返回 (sin, cos)
    let (sin_phi, cos_phi) = phi.sin_cos();
    let dx2 = (from[0] - to[0]) / 2.0;
    let dy2 = (from[1] - to[1]) / 2.0;
    let x1 = cos_phi * dx2 + sin_phi * dy2;
    let y1 = -sin_phi * dx2 + cos_phi * dy2;
    let lambda = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    let (rx, ry) = if lambda > 1.0 {
        let s = lambda.sqrt();
        (rx * s, ry * s)
    } else {
        (rx, ry)
    };
    let sign = if large_arc != sweep { 1.0 } else { -1.0 };
    let denom = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let co = if denom.abs() < f32::EPSILON {
        0.0
    } else {
        sign * ((rx * rx * ry * ry - denom) / denom).max(0.0).sqrt()
    };
    let cxp = co * rx * y1 / ry;
    let cyp = -co * ry * x1 / rx;
    let cx = cos_phi * cxp - sin_phi * cyp + (from[0] + to[0]) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (from[1] + to[1]) / 2.0;

    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            -a
        } else {
            a
        }
    };
    let theta1 = angle(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dtheta = angle(
        (x1 - cxp) / rx,
        (y1 - cyp) / ry,
        (-x1 - cxp) / rx,
        (-y1 - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f32::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f32::consts::TAU;
    }

    let segs = (dtheta.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let delta = dtheta / segs as f32;
    let t = 4.0 / 3.0 * (delta / 4.0).tan();
    let mut out = Vec::with_capacity(segs);
    let mut th = theta1;
    let mut p0 = from;
    for _ in 0..segs {
        // sin_cos 返回 (sin, cos)
        let (sin1, cos1) = th.sin_cos();
        let (sin2, cos2) = (th + delta).sin_cos();
        let e1x = cos_phi * rx * cos2 - sin_phi * ry * sin2 + cx;
        let e1y = sin_phi * rx * cos2 + cos_phi * ry * sin2 + cy;
        let d1x = t * (-cos_phi * rx * sin1 - sin_phi * ry * cos1);
        let d1y = t * (cos_phi * rx * cos1 - sin_phi * ry * sin1);
        let d2x = t * (-cos_phi * rx * sin2 - sin_phi * ry * cos2);
        let d2y = t * (cos_phi * rx * cos2 - sin_phi * ry * sin2);
        let seg_to = [e1x, e1y];
        out.push((
            [p0[0] + d1x, p0[1] + d1y],
            [seg_to[0] - d2x, seg_to[1] - d2y],
            seg_to,
        ));
        p0 = seg_to;
        th += delta;
    }
    out
}

// ===== 词法 =====

struct Tokenizer<'a> {
    chars: std::str::CharIndices<'a>,
    peeked: Option<(usize, char)>,
    /// 上一命令原字符（数字在途 = 隐式重复同一命令；M/m 按规范映射为 L/l）。
    repeat: Option<char>,
}

impl<'a> Tokenizer<'a> {
    fn new(src: &'a str) -> Self {
        Self { chars: src.char_indices(), peeked: None, repeat: None }
    }

    fn set_repeat(&mut self, cmd: char) {
        self.repeat = Some(cmd);
    }

    fn bump(&mut self) -> Option<char> {
        match self.peeked.take() {
            Some((_, c)) => Some(c),
            None => self.chars.next().map(|(_, c)| c),
        }
    }

    fn peek(&mut self) -> Option<char> {
        if self.peeked.is_none() {
            self.peeked = self.chars.next();
        }
        self.peeked.map(|(_, c)| c)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | ',' | '\n' | '\r' | '\t')) {
            self.bump();
        }
    }

    /// 下一条命令；无显式命令而数字在途时按隐式重复解析
    ///（M/m 的隐式重复按规范是 L/l，相对性由大小写给出）。
    fn command(&mut self) -> Option<(char, bool)> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.bump();
                self.repeat = None;
                Some((c, c.is_ascii_lowercase()))
            }
            Some(c) if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' => {
                let eff = match self.repeat? {
                    'M' => 'L',
                    'm' => 'l',
                    other => other,
                };
                Some((eff, eff.is_ascii_lowercase()))
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' => {}
            _ => return None,
        }
        let mut buf = String::new();
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_digit() || c == 'e' || c == 'E' => {
                    buf.push(self.bump().unwrap());
                }
                // 小数点：首个进入当前数；再次出现 = 新数的开头（SVG 允许 "1.5.5" 紧凑写法）
                Some('.') if !buf.contains('.') => {
                    buf.push(self.bump().unwrap());
                }
                // 符号：数的开头（含 ".5" 的负号）或指数符号（1e-3）
                Some('-' | '+') if buf.is_empty() || buf.ends_with(['e', 'E']) => {
                    buf.push(self.bump().unwrap());
                }
                _ => break,
            }
        }
        buf.parse().ok()
    }

    fn point(&mut self, relative: bool, pen: [f32; 2]) -> Option<[f32; 2]> {
        let x = self.number()?;
        let y = self.number()?;
        Some(if relative {
            [pen[0] + x, pen[1] + y]
        } else {
            [x, y]
        })
    }

    fn cubic(&mut self, relative: bool, pen: [f32; 2]) -> Option<([f32; 2], [f32; 2], [f32; 2])> {
        let c1 = self.point(relative, pen)?;
        let c2 = self.point(relative, pen)?;
        let to = self.point(relative, pen)?;
        Some((c1, c2, to))
    }

    fn pair2(&mut self, relative: bool, pen: [f32; 2]) -> Option<([f32; 2], [f32; 2])> {
        let a = self.point(relative, pen)?;
        let b = self.point(relative, pen)?;
        Some((a, b))
    }

    #[allow(clippy::type_complexity)]
    fn arc(
        &mut self,
        relative: bool,
        pen: [f32; 2],
    ) -> Option<(f32, f32, f32, bool, bool, [f32; 2])> {
        let rx = self.number()?;
        let ry = self.number()?;
        let rot = self.number()?;
        // 旗标为单字符 0/1，规范允许与后续数字连写
        let large = self.flag()?;
        let sweep = self.flag()?;
        let to = self.point(relative, pen)?;
        Some((rx, ry, rot, large, sweep, to))
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip_ws();
        match self.bump() {
            Some('0') => Some(false),
            Some('1') => Some(true),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(d: &str) -> Vec<[f32; 2]> {
        flatten_path(d).into_iter().flatten().collect()
    }

    /// 紧凑小数（"1.704.706"）按 SVG 词法切成两个数。
    #[test]
    fn compact_decimals_split_into_two_numbers() {
        let p = pts("M1.704.706 2 3");
        assert_eq!(p.len(), 2);
        assert!((p[0][0] - 1.704).abs() < 1e-5 && (p[0][1] - 0.706).abs() < 1e-5);
        assert!((p[1][0] - 2.0).abs() < 1e-5 && (p[1][1] - 3.0).abs() < 1e-5);
    }

    /// m 的隐式重复按规范是相对 lineto。
    #[test]
    fn implicit_repeat_after_moveto_is_relative_lineto() {
        let p = pts("m15 9-6 6");
        assert_eq!(p.len(), 2);
        assert!((p[1][0] - 9.0).abs() < 1e-5 && (p[1][1] - 15.0).abs() < 1e-5);
    }

    /// A 命令的隐式重复：一条 `a` 后跟多组 7 参数全部解析。
    #[test]
    fn arc_implicit_repeat_parses_all_groups() {
        // settings 的齿轮弧前两组（每组 7 参数）；每组 180° 弧 = 2 段贝塞尔 × 10 点
        let p = pts("a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915");
        assert_eq!(p.len(), 41);
    }

    /// 圆弧端点参数化：单段 90° 弧终点落在期望坐标。
    #[test]
    fn arc_endpoint_reaches_target() {
        let p = pts("M0 0A10 10 0 0 1 10 10");
        let last = p.last().unwrap();
        assert!((last[0] - 10.0).abs() < 1e-4 && (last[1] - 10.0).abs() < 1e-4);
    }
}
