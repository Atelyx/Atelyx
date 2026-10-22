//! 多维表格纯逻辑：字段类型口径、列宽估算、状态栏列计算、选区归约、剪贴板 TSV、
//! 单元格样式与行列结构操作。模型真源 = `vault::TableFile`，本模块只做模型内变换，
//! 不做任何 I/O；展示层语义（文本换行估算等）归壳侧。

use serde_json::Value;

use crate::vault::{TableFile, TableField, TableRow};

/// 状态栏自动计算类型显示名。
pub fn calc_type_label(t: &str) -> &'static str {
    match t {
        "sum" => "求和",
        "avg" => "平均",
        "max" => "最大",
        "min" => "最小",
        "count" => "计数",
        _ => "",
    }
}

/// 各字段类型可用的自动计算（数字类统计，其余计数非空值）。
pub fn calc_types_for(field_type: &str) -> &'static [&'static str] {
    match field_type {
        "number" | "duration" => &["sum", "avg", "max", "min", "count"],
        _ => &["count"],
    }
}

/// 列宽拖拽调整的下限 / 上限（px）。
pub const MIN_COL_WIDTH: f64 = 120.0;
pub const MAX_COL_WIDTH: f64 = 360.0;
/// 行高拖拽调整的下限 / 上限（px；行首操作区高 32px）。
pub const MIN_ROW_HEIGHT: f64 = 32.0;
pub const MAX_ROW_HEIGHT: f64 = 600.0;

/// 字符显示宽度单位：CJK 标点/汉字/全角形式 = 2，其余 = 1（列宽/单元格内容估算共用口径）。
fn char_units(ch: char) -> f64 {
    match ch as u32 {
        0x3000..=0x303F | 0x4E00..=0x9FFF | 0xFF00..=0xFFEF => 2.0,
        _ => 1.0,
    }
}

fn text_units(text: &str) -> f64 {
    text.chars().map(char_units).sum()
}

/// 列宽默认值：按字段名称字数自适应（CJK 双宽，每单位 7px + 边距 24px），钳制区间内；
/// 用户拖拽调整后存字段 `width` 覆盖。
pub fn field_default_width(name: &str) -> f64 {
    (text_units(name) * 7.0 + 24.0).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
}

/// 单个单元格内容的估算宽度（与 field_default_width 同口径；image 缩略图固定 72px）。
fn cell_content_width(v: Option<&Value>) -> f64 {
    let Some(v) = v else { return 0.0 };
    if is_image_value(v) {
        return 72.0;
    }
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(|f| f.to_string()).unwrap_or_default(),
        _ => String::new(),
    };
    text.split('\n')
        .map(|line| text_units(line) * 7.0 + 24.0)
        .fold(0.0, f64::max)
}

/// 列宽自适应：列内容最大估算宽且不小于字段名宽度，钳制区间内；空列 = 名称宽（与清除手动宽度等效）。
pub fn column_auto_width(field: &TableField, rows: &[TableRow]) -> f64 {
    let name_width = field_default_width(&field.name);
    let content_width = rows.iter().fold(0.0f64, |acc, r| {
        acc.max(cell_content_width(r.values.get(&field.id)))
    });
    name_width.max(content_width).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
}

/// 新建空表格（自带一个「名称」文本字段，空白表无字段难用）。
pub fn new_table_file(title: &str) -> TableFile {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    TableFile {
        schema: crate::vault::TABLE_SCHEMA.into(),
        id: nanoid::nanoid!(),
        title: title.to_string(),
        fields: vec![TableField {
            id: nanoid::nanoid!(),
            name: "名称".into(),
            field_type: "text".into(),
            options: None,
            width: None,
            calc_type: None,
        }],
        rows: vec![],
        created_at: now,
        updated_at: now,
    }
}

// ===== 单元格值工具 =====

/// 值是否为图片形态（新 `{images}` 对象或旧 `string[]`）。
pub(crate) fn is_image_value(v: &Value) -> bool {
    v.is_array() || v.get("images").is_some_and(|i| i.is_array())
}

/// 图片条目列表（双形态兼容）：新形态 `{ images: [...] }` / 旧形态 `string[]` → 字符串列表。
pub fn image_entries(v: &Value) -> Vec<&str> {
    v.as_array()
        .or_else(|| v.get("images").and_then(|i| i.as_array()))
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default()
}

/// 图片展示模式：`Some("grid")` = 九宫格，None = 轮播（缺省）。
pub fn image_display(v: &Value) -> Option<&str> {
    v.get("display").and_then(|d| d.as_str()).filter(|d| *d == "grid")
}

/// 图片值归一化：旧形态 `string[]` → `{ images: [...] }`；对象形态剔除非字符串条目；
/// 其余形态 → 空图片值。`display` 键存在时原样保留（返回值恒为对象形态）。
fn normalize_image_value(v: &Value) -> Value {
    let source = if let Some(a) = v.as_array() {
        a
    } else if let Some(a) = v.get("images").and_then(|i| i.as_array()) {
        a
    } else {
        return serde_json::json!({ "images": [] });
    };
    let images: Vec<Value> = source.iter().filter(|x| x.is_string()).cloned().collect();
    let mut out = serde_json::json!({ "images": images });
    if let Some(d) = v.get("display") {
        out["display"] = d.clone();
    }
    out
}

/// 行值归一化：旧形态图片值（`string[]`）就地归一化为 `{ images }`（load 大表免全量重建）。
pub fn normalize_table_row(row: &mut TableRow) {
    let dirty: Vec<String> = row
        .values
        .iter()
        .filter(|(_, v)| v.is_array())
        .map(|(k, _)| k.clone())
        .collect();
    for k in dirty {
        let normalized = normalize_image_value(&row.values[&k]);
        row.values.insert(k, normalized);
    }
}

/// 值是否为「非空」（count 口径；image 按图片数，字符串按非空白）。
fn is_non_empty_value(v: Option<&Value>) -> bool {
    match v {
        None => false,
        Some(v) if v.is_number() => true,
        Some(v) if v.is_string() => v.as_str().is_some_and(|s| !s.trim().is_empty()),
        Some(v) => !image_entries(v).is_empty(),
    }
}

/// 单元格 → 剪贴板文本（image = 空；number/duration = 数值显示串，与视图渲染同口径；
/// text/singleSelect = 原串）。
fn cell_to_clipboard_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f.to_string()).unwrap_or_else(|| n.to_string()),
        _ => String::new(),
    }
}

/// 数值写盘形态：整数值以整数 Number 落盘（与既有文件口径一致，避免 f64 序列化出 `1.0`
/// 与存量整数形态判不等导致的无谓置脏）；小数保持 f64。
pub fn number_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        serde_json::json!(n)
    }
}

/// 单元格值相等（数组按项比较；空值 ≈ 缺键：序列化丢空/缺省键不误判为外部修改；
/// 图片值空数组无模式 ≈ 缺键，两侧各自归一化后比较）。
pub fn cell_value_equal(a: Option<&Value>, b: Option<&Value>) -> bool {
    let empty_image = |v: &Value| {
        is_image_value(v) && image_entries(v).is_empty() && image_display(v).is_none()
    };
    match (a, b) {
        (None, None) => true,
        (Some(v), None) | (None, Some(v)) => empty_image(v) || v.as_str() == Some(""),
        (Some(x), Some(y)) => {
            if is_image_value(x) || is_image_value(y) {
                if !is_image_value(x) || !is_image_value(y) {
                    return false;
                }
                if image_display(x) != image_display(y) {
                    return false;
                }
                let nx = normalize_image_value(x);
                let ny = normalize_image_value(y);
                return image_entries(&nx) == image_entries(&ny);
            }
            x == y
        }
    }
}

/// 写单元格值（None = 清空删键）；新值与旧值相等时不动（防无意义置脏）。
/// 返回是否实际变化。
pub fn set_cell_value(row: &mut TableRow, field_id: &str, v: Option<Value>) -> bool {
    let old = row.values.get(field_id);
    if cell_value_equal(old, v.as_ref()) {
        return false;
    }
    match v {
        Some(v) => {
            row.values.insert(field_id.to_string(), v);
        }
        None => {
            row.values.remove(field_id);
        }
    }
    true
}

/// 数值显示：整数原样，小数保留两位去尾 0（状态栏计算口径）；round 链路可能产 -0.0，加 0.0 规格化。
fn format_calc_number(n: f64) -> String {
    format!("{}", (n * 100.0).round() / 100.0 + 0.0)
}

/// 状态栏列自动计算：按字段 calcType 统计全列（未设返回 None）。
/// sum/count 无数据为 0；avg/max/min 无数据为「—」。
pub fn compute_column_calc(field: &TableField, rows: &[TableRow]) -> Option<String> {
    let calc = field.calc_type.as_deref()?;
    let values: Vec<Option<&Value>> = rows.iter().map(|r| r.values.get(&field.id)).collect();
    match calc {
        "count" => Some(format_calc_number(values.iter().filter(|v| is_non_empty_value(**v)).count() as f64)),
        "sum" | "avg" | "max" | "min" => {
            let nums: Vec<f64> = values.into_iter().filter_map(|v| v.and_then(Value::as_f64)).collect();
            let r = match calc {
                "sum" => nums.iter().sum::<f64>(),
                "avg" if !nums.is_empty() => nums.iter().sum::<f64>() / nums.len() as f64,
                "max" if !nums.is_empty() => nums.iter().copied().fold(f64::MIN, f64::max),
                "min" if !nums.is_empty() => nums.iter().copied().fold(f64::MAX, f64::min),
                _ => return Some("—".to_string()),
            };
            Some(format_calc_number(r))
        }
        _ => None,
    }
}

// ===== 选区 =====

/// 表格选中范围（互斥；存 id 不存下标：行/字段增删后选区不悬空）。
#[derive(Clone, Debug, PartialEq)]
pub enum TableSelection {
    Cell { row_id: String, field_id: String },
    Range { anchor_row_id: String, anchor_field_id: String, row_id: String, field_id: String },
    Row(String),
    Column(String),
    All,
}

/// 矩形区域（行/列下标区间，闭区间）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub row_start: usize,
    pub row_end: usize,
    pub col_start: usize,
    pub col_end: usize,
}

impl Region {
    pub fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.row_start && row <= self.row_end && col >= self.col_start && col <= self.col_end
    }
}

/// 选中范围 → 矩形区域（复制/粘贴/清空/样式应用统一复用）；id 失效（行/字段已删）→ None。
pub fn region_of(sel: Option<&TableSelection>, fields: &[TableField], rows: &[TableRow]) -> Option<Region> {
    let sel = sel?;
    let last_row = rows.len().checked_sub(1)?;
    let last_col = fields.len().checked_sub(1)?;
    Some(match sel {
        TableSelection::Cell { row_id, field_id } => {
            let r = rows.iter().position(|x| &x.id == row_id)?;
            let c = fields.iter().position(|x| &x.id == field_id)?;
            Region { row_start: r, row_end: r, col_start: c, col_end: c }
        }
        TableSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id } => {
            let r1 = rows.iter().position(|x| &x.id == anchor_row_id)?;
            let r2 = rows.iter().position(|x| &x.id == row_id)?;
            let c1 = fields.iter().position(|x| &x.id == anchor_field_id)?;
            let c2 = fields.iter().position(|x| &x.id == field_id)?;
            Region {
                row_start: r1.min(r2),
                row_end: r1.max(r2),
                col_start: c1.min(c2),
                col_end: c1.max(c2),
            }
        }
        TableSelection::Row(id) => {
            let r = rows.iter().position(|x| &x.id == id)?;
            Region { row_start: r, row_end: r, col_start: 0, col_end: last_col }
        }
        TableSelection::Column(id) => {
            let c = fields.iter().position(|x| &x.id == id)?;
            Region { row_start: 0, row_end: last_row, col_start: c, col_end: c }
        }
        TableSelection::All => Region { row_start: 0, row_end: last_row, col_start: 0, col_end: last_col },
    })
}

/// 选区 → 跨端 JSON：存稳定 id（行/字段增删后对端自行失效），形态与插件表格快照的选区一致。
pub fn selection_to_json(sel: &TableSelection) -> Value {
    match sel {
        TableSelection::Cell { row_id, field_id } => Value::Object(
            [
                ("kind".to_string(), Value::from("cell")),
                ("rowId".to_string(), Value::from(row_id.as_str())),
                ("fieldId".to_string(), Value::from(field_id.as_str())),
            ]
            .into_iter()
            .collect(),
        ),
        TableSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id } => Value::Object(
            [
                ("kind".to_string(), Value::from("range")),
                ("anchorRowId".to_string(), Value::from(anchor_row_id.as_str())),
                ("anchorFieldId".to_string(), Value::from(anchor_field_id.as_str())),
                ("rowId".to_string(), Value::from(row_id.as_str())),
                ("fieldId".to_string(), Value::from(field_id.as_str())),
            ]
            .into_iter()
            .collect(),
        ),
        TableSelection::Row(id) => Value::Object(
            [
                ("kind".to_string(), Value::from("row")),
                ("rowId".to_string(), Value::from(id.as_str())),
            ]
            .into_iter()
            .collect(),
        ),
        TableSelection::Column(id) => Value::Object(
            [
                ("kind".to_string(), Value::from("column")),
                ("fieldId".to_string(), Value::from(id.as_str())),
            ]
            .into_iter()
            .collect(),
        ),
        TableSelection::All => Value::Object(
            [("kind".to_string(), Value::from("all"))].into_iter().collect(),
        ),
    }
}

/// 跨端 JSON → 选区：字段缺失或未知形态 = None（对端版本更新/载荷损坏时丢弃该条）。
pub fn selection_from_json(value: &Value) -> Option<TableSelection> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    Some(match value.get("kind")?.as_str()? {
        "cell" => TableSelection::Cell { row_id: text("rowId")?, field_id: text("fieldId")? },
        "range" => TableSelection::Range {
            anchor_row_id: text("anchorRowId")?,
            anchor_field_id: text("anchorFieldId")?,
            row_id: text("rowId")?,
            field_id: text("fieldId")?,
        },
        "row" => TableSelection::Row(text("rowId")?),
        "column" => TableSelection::Column(text("fieldId")?),
        "all" => TableSelection::All,
        _ => return None,
    })
}

/// 区域 → TSV（行 = \n、列 = \t）：单格 = 原值（多行文本原样无损）；
/// 多格 = 值内嵌 \t/\n 压成空格保 TSV 结构完整。
pub fn build_region_tsv(fields: &[TableField], rows: &[TableRow], region: &Region) -> String {
    let multi = region.row_start != region.row_end || region.col_start != region.col_end;
    let cell_text = |r: usize, c: usize| -> String {
        let Some(field) = fields.get(c) else { return String::new() };
        let t = rows
            .get(r)
            .and_then(|row| row.values.get(&field.id))
            .map_or_else(String::new, |v| cell_to_clipboard_text(Some(v)));
        if multi {
            t.replace(['\t', '\n'], " ")
        } else {
            t
        }
    };
    (region.row_start..=region.row_end)
        .map(|r| {
            (region.col_start..=region.col_end)
                .map(|c| cell_text(r, c))
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 剪贴板 TSV → 值网格：\r\n 归一、制表符分列，末尾换行的空尾行剔除。
/// 纯 TSV 不处理引号/内嵌换行（外部 Excel 带内嵌换行的格会拆行）。
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<Vec<String>> =
        normalized.split('\n').map(|l| l.split('\t').map(|c| c.to_string()).collect()).collect();
    if lines.len() > 1 && lines.last().is_some_and(|l| l.len() == 1 && l[0].is_empty()) {
        lines.pop();
    }
    lines
}

/// 粘贴值强转：text 原串（空 → None）；number/duration 数值化（非有限数/空 → None）。
fn coerce_paste_value(raw: &str, field: &TableField) -> Option<Value> {
    if field.field_type == "text" {
        return if raw.is_empty() { None } else { Some(Value::String(raw.to_string())) };
    }
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite()).map(number_value)
}

/// 把粘贴网格应用到锚点 (anchor_row, anchor_col) 右下展开：越界自动补行（空行）/补列
/// （text 字段「字段N」）；按目标字段类型强转（singleSelect 命中选项才写否则跳过、image 跳过）；
/// 空格清空目标格。返回是否实际写入（全跳过/全空 = false，调用方丢弃补出的空行/空列不置脏）。
pub fn apply_paste_grid(
    fields: &mut Vec<TableField>,
    rows: &mut Vec<TableRow>,
    anchor_row: usize,
    anchor_col: usize,
    grid: &[Vec<String>],
    new_id: impl Fn() -> String,
) -> bool {
    let need_cols = anchor_col + grid.iter().map(Vec::len).max().unwrap_or(0);
    if need_cols > fields.len() {
        for i in fields.len()..need_cols {
            fields.push(TableField {
                id: new_id(),
                name: format!("字段{}", i + 1),
                field_type: "text".into(),
                options: None,
                width: None,
                calc_type: None,
            });
        }
    }
    let original_fields = fields.len();
    let original_rows = rows.len();
    let mut any_changed = false;
    for (g, line) in grid.iter().enumerate() {
        let r = anchor_row + g;
        while rows.len() <= r {
            rows.push(TableRow { id: new_id(), values: Default::default(), height: None, styles: None });
        }
        let row = &mut rows[r];
        let mut row_changed = false;
        for (c, raw) in line.iter().enumerate() {
            let field = &fields[anchor_col + c];
            let next = match field.field_type.as_str() {
                "singleSelect" => {
                    if raw.is_empty() {
                        None
                    } else if field.options.as_ref().is_some_and(|o| o.contains(raw)) {
                        Some(Value::String(raw.clone()))
                    } else {
                        continue; // 非选项值：不覆盖不清空
                    }
                }
                "image" => continue,
                _ => coerce_paste_value(raw, field),
            };
            if set_cell_value(row, &field.id, next) {
                row_changed = true;
            }
        }
        if row_changed {
            any_changed = true;
        }
    }
    // 无实际写入（全跳过/全空）：丢弃补出的空行/空列，返回 false（调用方不置脏不入栈）
    if !any_changed {
        fields.truncate(original_fields);
        rows.truncate(original_rows);
        return false;
    }
    true
}

// ===== 单元格显示样式 =====

/// 单元格显示样式（styles[fieldId] 的强类型视图；只存非默认键，布尔缺省 = false）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellStyle {
    pub b: bool,
    pub i: bool,
    pub u: bool,
    pub s: bool,
    pub color: Option<String>,
    pub bg: Option<String>,
    pub font: Option<String>,
    pub size: Option<f64>,
}

/// 读单元格样式（缺 key = 默认样式）。
pub fn style_of(row: &TableRow, field_id: &str) -> CellStyle {
    let Some(map) = &row.styles else { return CellStyle::default() };
    let Some(v) = map.get(field_id) else { return CellStyle::default() };
    CellStyle {
        b: v.get("b").and_then(Value::as_bool).unwrap_or(false),
        i: v.get("i").and_then(Value::as_bool).unwrap_or(false),
        u: v.get("u").and_then(Value::as_bool).unwrap_or(false),
        s: v.get("s").and_then(Value::as_bool).unwrap_or(false),
        color: v.get("color").and_then(Value::as_str).map(str::to_string),
        bg: v.get("bg").and_then(Value::as_str).map(str::to_string),
        font: v.get("font").and_then(Value::as_str).map(str::to_string),
        size: v.get("size").and_then(Value::as_f64),
    }
}

/// 写单元格样式：全默认 → 删该格样式键；该行 styles 空 → 删 styles（保持文件干净）。
fn write_style(row: &mut TableRow, field_id: &str, st: &CellStyle) {
    let default = CellStyle::default();
    let map = row.styles.get_or_insert_with(Default::default);
    if *st == default {
        map.remove(field_id);
    } else {
        let mut v = serde_json::Map::new();
        if st.b {
            v.insert("b".into(), Value::Bool(true));
        }
        if st.i {
            v.insert("i".into(), Value::Bool(true));
        }
        if st.u {
            v.insert("u".into(), Value::Bool(true));
        }
        if st.s {
            v.insert("s".into(), Value::Bool(true));
        }
        if let Some(c) = &st.color {
            v.insert("color".into(), Value::String(c.clone()));
        }
        if let Some(c) = &st.bg {
            v.insert("bg".into(), Value::String(c.clone()));
        }
        if let Some(f) = &st.font {
            v.insert("font".into(), Value::String(f.clone()));
        }
        if let Some(s) = st.size {
            v.insert("size".into(), serde_json::json!(s));
        }
        map.insert(field_id.to_string(), Value::Object(v));
    }
    if map.is_empty() {
        row.styles = None;
    }
}

/// 样式补丁键位（Keep = 不动；Set(None) = 清除该键）。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum KeyOp<T> {
    #[default]
    Keep,
    Set(Option<T>),
}

impl<T> KeyOp<T> {
    fn is_keep(&self) -> bool {
        matches!(self, KeyOp::Keep)
    }
}

/// 样式补丁（对区域逐格应用；显式键才生效）。
#[derive(Clone, Debug, Default)]
pub struct StylePatch {
    pub b: KeyOp<bool>,
    pub i: KeyOp<bool>,
    pub u: KeyOp<bool>,
    pub s: KeyOp<bool>,
    pub color: KeyOp<String>,
    pub bg: KeyOp<String>,
    pub font: KeyOp<String>,
    pub size: KeyOp<f64>,
}

impl StylePatch {
    fn is_keep_all(&self) -> bool {
        self.b.is_keep() && self.i.is_keep() && self.u.is_keep() && self.s.is_keep()
            && self.color.is_keep() && self.bg.is_keep() && self.font.is_keep() && self.size.is_keep()
    }
}

fn apply_key<T: Clone>(dst: &mut Option<T>, op: &KeyOp<T>) {
    if let KeyOp::Set(v) = op {
        *dst = v.clone();
    }
}

/// 对区域逐格应用样式补丁（col 下标经 fields 映射到字段 id），返回是否实际变化
/// （无变化调用方不置脏不入栈）。
pub fn apply_style_patch(
    fields: &[TableField],
    rows: &mut [TableRow],
    region: &Region,
    patch: &StylePatch,
) -> bool {
    if patch.is_keep_all() {
        return false;
    }
    let mut changed = false;
    for row in rows.iter_mut().take(region.row_end + 1).skip(region.row_start) {
        for c in region.col_start..=region.col_end {
            let Some(field_id) = fields.get(c).map(|f| f.id.as_str()) else { continue };
            let before = style_of(row, field_id);
            let mut st = before.clone();
            if let KeyOp::Set(v) = &patch.b {
                st.b = v.unwrap_or(false);
            }
            if let KeyOp::Set(v) = &patch.i {
                st.i = v.unwrap_or(false);
            }
            if let KeyOp::Set(v) = &patch.u {
                st.u = v.unwrap_or(false);
            }
            if let KeyOp::Set(v) = &patch.s {
                st.s = v.unwrap_or(false);
            }
            apply_key(&mut st.color, &patch.color);
            apply_key(&mut st.bg, &patch.bg);
            apply_key(&mut st.font, &patch.font);
            apply_key(&mut st.size, &patch.size);
            if st != before {
                write_style(row, field_id, &st);
                changed = true;
            }
        }
    }
    changed
}

/// 可选值三态（选区汇总；Value(None) = 全体默认）。
#[derive(Clone, Debug, PartialEq)]
pub enum OptState<T> {
    Value(Option<T>),
    Mixed,
}

/// 选区样式汇总（格式工具栏三态来源）：逐属性判定——全体同值取该值，不一致 = Mixed。
pub fn style_summary(
    fields: &[TableField],
    rows: &[TableRow],
    region: &Region,
) -> StyleSummary {
    let cell = |r: usize, c: usize| -> CellStyle {
        rows.get(r).and_then(|row| fields.get(c).map(|f| style_of(row, &f.id))).unwrap_or_default()
    };
    let uniform = |pick: &dyn Fn(&CellStyle) -> OptState<bool>| -> OptState<bool> {
        let base = pick(&cell(region.row_start, region.col_start));
        for r in region.row_start..=region.row_end {
            for c in region.col_start..=region.col_end {
                if pick(&cell(r, c)) != base {
                    return OptState::Mixed;
                }
            }
        }
        base
    };
    StyleSummary {
        bold: uniform(&|st| OptState::Value(Some(st.b))),
        italic: uniform(&|st| OptState::Value(Some(st.i))),
        underline: uniform(&|st| OptState::Value(Some(st.u))),
        strike: uniform(&|st| OptState::Value(Some(st.s))),
        color: opt_uniform(&cell, region, |st| st.color.clone()),
        bg: opt_uniform(&cell, region, |st| st.bg.clone()),
        font: opt_uniform(&cell, region, |st| st.font.clone()),
        size: opt_uniform(&cell, region, |st| st.size),
    }
}

fn opt_uniform<T: PartialEq + Clone>(
    cell: &dyn Fn(usize, usize) -> CellStyle,
    region: &Region,
    pick: impl Fn(&CellStyle) -> Option<T>,
) -> OptState<T> {
    let base = pick(&cell(region.row_start, region.col_start));
    for r in region.row_start..=region.row_end {
        for c in region.col_start..=region.col_end {
            if pick(&cell(r, c)) != base {
                return OptState::Mixed;
            }
        }
    }
    OptState::Value(base)
}

/// 选区样式汇总结果。
#[derive(Clone, Debug)]
pub struct StyleSummary {
    pub bold: OptState<bool>,
    pub italic: OptState<bool>,
    pub underline: OptState<bool>,
    pub strike: OptState<bool>,
    pub color: OptState<String>,
    pub bg: OptState<String>,
    pub font: OptState<String>,
    pub size: OptState<f64>,
}

// ===== 行列结构操作 =====

/// 行尾追加空行（返回新行 id）。
pub fn add_row(table: &mut TableFile, id: String) -> String {
    table.rows.push(TableRow { id: id.clone(), values: Default::default(), height: None, styles: None });
    id
}

/// 复制行（值 + 样式全拷，浅拷贝共享字符串），插入原行下方；目标不存在返回 false。
pub fn duplicate_row(table: &mut TableFile, row_id: &str, id: String) -> bool {
    let Some(i) = table.rows.iter().position(|r| r.id == row_id) else { return false };
    let mut clone = table.rows[i].clone();
    clone.id = id;
    table.rows.insert(i + 1, clone);
    true
}

/// 删除行（返回删除数；幂等）。
pub fn remove_rows(table: &mut TableFile, ids: &[String]) -> usize {
    let before = table.rows.len();
    table.rows.retain(|r| !ids.contains(&r.id));
    before - table.rows.len()
}

/// 行移动到目标插入位（target = 原数组语义的插入下标，越界钳制到表尾；from < target 时 -1
/// 校正；校正后与原位相等 = 无操作返回 false，不入栈）。
pub fn move_row(table: &mut TableFile, row_id: &str, target: usize) -> bool {
    let Some(from) = table.rows.iter().position(|r| r.id == row_id) else { return false };
    let target = target.min(table.rows.len());
    let corrected = if from < target { target - 1 } else { target };
    if corrected == from {
        return false;
    }
    let row = table.rows.remove(from);
    table.rows.insert(corrected, row);
    true
}

/// 在下标处插入字段（下标钳制到现有长度内）。
pub fn insert_field(table: &mut TableFile, index: usize, id: String, name: &str, field_type: &str) {
    let index = index.min(table.fields.len());
    table.fields.insert(
        index,
        TableField {
            id,
            name: name.to_string(),
            field_type: field_type.to_string(),
            options: (field_type == "singleSelect").then(Default::default),
            width: None,
            calc_type: None,
        },
    );
}

/// 删除字段：同步清全行 values/styles 该键。
pub fn remove_field(table: &mut TableFile, field_id: &str) {
    table.fields.retain(|f| f.id != field_id);
    for row in &mut table.rows {
        row.values.remove(field_id);
        if let Some(map) = &mut row.styles {
            map.remove(field_id);
            if map.is_empty() {
                row.styles = None;
            }
        }
    }
}

/// 字段改名（trim 后空名保留原名）；返回是否变化。
pub fn rename_field(table: &mut TableFile, field_id: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let Some(f) = table.fields.iter_mut().find(|f| f.id == field_id) else { return false };
    if f.name == name {
        return false;
    }
    f.name = name.to_string();
    true
}

/// 字段改类型：仅改 type；calcType 不属新类型可用集则清；转入单选补空选项集；
/// 单元格值不转换。返回是否变化。
pub fn retype_field(table: &mut TableFile, field_id: &str, field_type: &str) -> bool {
    let Some(f) = table.fields.iter_mut().find(|f| f.id == field_id) else { return false };
    if f.field_type == field_type {
        return false;
    }
    f.field_type = field_type.to_string();
    if field_type == "singleSelect" {
        if f.options.is_none() {
            f.options = Some(Vec::new());
        }
    } else {
        f.options = None;
    }
    if let Some(calc) = &f.calc_type {
        if !calc_types_for(field_type).contains(&calc.as_str()) {
            f.calc_type = None;
        }
    }
    true
}

/// 整组替换单选选项；返回是否变化。
pub fn set_field_options(table: &mut TableFile, field_id: &str, options: Vec<String>) -> bool {
    let Some(f) = table.fields.iter_mut().find(|f| f.id == field_id) else { return false };
    let options: Vec<String> = options.into_iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect();
    if f.options.as_ref() == Some(&options) {
        return false;
    }
    f.options = Some(options);
    true
}

/// 设置列宽（None = 清除手动宽度恢复默认）；返回是否变化。
pub fn set_field_width(table: &mut TableFile, field_id: &str, width: Option<f64>) -> bool {
    let Some(f) = table.fields.iter_mut().find(|f| f.id == field_id) else { return false };
    let width = width.map(|w| w.clamp(MIN_COL_WIDTH, MAX_COL_WIDTH));
    if f.width == width {
        return false;
    }
    f.width = width;
    true
}

/// 设置行高（None = 清除恢复自然撑开）；返回是否变化。
pub fn set_row_height(table: &mut TableFile, row_id: &str, height: Option<f64>) -> bool {
    let Some(r) = table.rows.iter_mut().find(|r| r.id == row_id) else { return false };
    let height = height.map(|h| h.clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT));
    if r.height == height {
        return false;
    }
    r.height = height;
    true
}

/// 行高自适应：清除指定行手动行高（无手动值 = no-op）；返回是否变化。
pub fn clear_row_heights(table: &mut TableFile, row_ids: &[String]) -> bool {
    let mut changed = false;
    for r in &mut table.rows {
        if row_ids.contains(&r.id) && r.height.is_some() {
            r.height = None;
            changed = true;
        }
    }
    changed
}

/// 设置状态栏计算类型（None = 无计算）；返回是否变化。
pub fn set_calc_type(table: &mut TableFile, field_id: &str, calc: Option<&str>) -> bool {
    let Some(f) = table.fields.iter_mut().find(|f| f.id == field_id) else { return false };
    if f.calc_type.as_deref() == calc {
        return false;
    }
    f.calc_type = calc.map(str::to_string);
    true
}

/// 撤销快照（字段 + 行集克隆；恢复 = 整体回写）。
pub type TableSnapshot = (Vec<TableField>, Vec<TableRow>);

pub fn restore(table: &mut TableFile, snap: TableSnapshot) {
    table.fields = snap.0;
    table.rows = snap.1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::TableFile;

    #[test]
    fn 选区跨端json往返且未知形态丢弃() {
        let cases = [
            TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() },
            TableSelection::Range {
                anchor_row_id: "r1".into(),
                anchor_field_id: "f1".into(),
                row_id: "r3".into(),
                field_id: "f2".into(),
            },
            TableSelection::Row("r2".into()),
            TableSelection::Column("f3".into()),
            TableSelection::All,
        ];
        for sel in cases {
            let wire = selection_to_json(&sel);
            assert_eq!(selection_from_json(&wire).as_ref(), Some(&sel));
        }
        assert_eq!(
            selection_to_json(&TableSelection::Column("f3".into())),
            serde_json::json!({ "kind": "column", "fieldId": "f3" })
        );
        for bad in [
            serde_json::json!({ "kind": "cell" }),
            serde_json::json!({ "kind": "node", "nodeId": "n1" }),
            serde_json::json!("cell"),
            serde_json::Value::Null,
        ] {
            assert!(selection_from_json(&bad).is_none(), "应丢弃：{bad}");
        }
    }

    fn field(id: &str, name: &str, field_type: &str) -> TableField {
        TableField {
            id: id.into(),
            name: name.into(),
            field_type: field_type.into(),
            options: None,
            width: None,
            calc_type: None,
        }
    }

    fn row(id: &str, values: Vec<(&str, Value)>) -> TableRow {
        TableRow {
            id: id.into(),
            values: values.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            height: None,
            styles: None,
        }
    }

    fn table(fields: Vec<TableField>, rows: Vec<TableRow>) -> TableFile {
        TableFile {
            schema: crate::vault::TABLE_SCHEMA.into(),
            id: "t".into(),
            title: "表格".into(),
            fields,
            rows,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn number_value_normalizes_integers() {
        // 整数以整数 Number 落盘：与存量文件形态判等，避免 f64 `1.0` 的无谓置脏
        assert_eq!(number_value(1.0), serde_json::json!(1));
        assert_eq!(number_value(-3.0), serde_json::json!(-3));
        assert_eq!(number_value(1.5), serde_json::json!(1.5));
        assert_eq!(
            number_value(1.0),
            serde_json::from_str::<Value>("1").expect("整数形态")
        );
    }

    #[test]
    fn clipboard_number_uses_display_form() {
        assert_eq!(cell_to_clipboard_text(Some(&number_value(1.0))), "1");
        assert_eq!(cell_to_clipboard_text(Some(&number_value(1.5))), "1.5");
    }

    #[test]
    fn normalize_image_value_preserves_display() {
        let v = serde_json::json!({ "images": ["a.png"], "display": "grid" });
        assert_eq!(normalize_image_value(&v), v);
        let legacy = serde_json::json!(["a.png", 3]);
        assert_eq!(
            normalize_image_value(&legacy),
            serde_json::json!({ "images": ["a.png"] })
        );
    }

    #[test]
    fn default_width_clamps_and_counts_cjk_double() {
        assert_eq!(field_default_width(""), MIN_COL_WIDTH);
        assert_eq!(field_default_width("abcdefgh"), MIN_COL_WIDTH); // 8×7+24 = 80 → 钳 120
        assert_eq!(field_default_width("名称"), MIN_COL_WIDTH); // 2×7+24 = 38 → 钳 120
        assert_eq!(field_default_width(&"名".repeat(60)), MAX_COL_WIDTH);
    }

    #[test]
    fn column_auto_width_uses_content_max_and_name_floor() {
        let f = field("f", "短", "text");
        let rows = vec![row("r1", vec![("f", serde_json::json!("一行"))]), row("r2", vec![("f", serde_json::json!("ab\ncd"))])];
        // 内容最大 4 units ×7 + 24 = 52 < 名称宽 → 取名称宽
        assert_eq!(column_auto_width(&f, &rows), field_default_width("短"));
        let f2 = field("f", "短", "text");
        let long: String = std::iter::repeat_n('a', 100).collect();
        let rows2 = vec![row("r1", vec![("f", serde_json::json!(long))])];
        assert_eq!(column_auto_width(&f2, &rows2), MAX_COL_WIDTH);
    }

    #[test]
    fn column_calc_semantics() {
        let mut f = field("f", "n", "number");
        let rows = vec![
            row("r1", vec![("f", serde_json::json!(1.5))]),
            row("r2", vec![("f", serde_json::json!(2))]),
            row("r3", vec![]), // 空 = 不参与数字统计
        ];
        f.calc_type = Some("sum".into());
        assert_eq!(compute_column_calc(&f, &rows).as_deref(), Some("3.5"));
        f.calc_type = Some("avg".into());
        assert_eq!(compute_column_calc(&f, &rows).as_deref(), Some("1.75"));
        f.calc_type = Some("max".into());
        assert_eq!(compute_column_calc(&f, &rows).as_deref(), Some("2"));
        f.calc_type = Some("min".into());
        assert_eq!(compute_column_calc(&f, &rows).as_deref(), Some("1.5"));
        f.calc_type = Some("count".into());
        assert_eq!(compute_column_calc(&f, &rows).as_deref(), Some("2"));
        let empty: Vec<TableRow> = vec![];
        f.calc_type = Some("avg".into());
        assert_eq!(compute_column_calc(&f, &empty).as_deref(), Some("—"));
        f.calc_type = Some("sum".into());
        assert_eq!(compute_column_calc(&f, &empty).as_deref(), Some("0"));
        f.calc_type = None;
        assert_eq!(compute_column_calc(&f, &rows), None);
    }

    #[test]
    fn region_of_maps_selections() {
        let fields = vec![field("f1", "a", "text"), field("f2", "b", "text")];
        let rows = vec![row("r1", vec![]), row("r2", vec![]), row("r3", vec![])];
        let sel = TableSelection::Range {
            anchor_row_id: "r3".into(),
            anchor_field_id: "f1".into(),
            row_id: "r1".into(),
            field_id: "f2".into(),
        };
        let region = region_of(Some(&sel), &fields, &rows).unwrap();
        assert_eq!(region, Region { row_start: 0, row_end: 2, col_start: 0, col_end: 1 });
        let all = region_of(Some(&TableSelection::All), &fields, &rows).unwrap();
        assert_eq!(all, Region { row_start: 0, row_end: 2, col_start: 0, col_end: 1 });
        assert!(region_of(Some(&TableSelection::Row("失效".into())), &fields, &rows).is_none());
    }

    #[test]
    fn tsv_roundtrip_and_paste_semantics() {
        let fields = vec![field("f1", "a", "text"), field("f2", "b", "number")];
        let rows = vec![row("r1", vec![("f1", serde_json::json!("多行\n文本")), ("f2", serde_json::json!(3))])];
        let region = Region { row_start: 0, row_end: 0, col_start: 0, col_end: 0 };
        let tsv = build_region_tsv(&fields, &rows, &region);
        assert_eq!(tsv, "多行\n文本");

        let mut f2 = fields.clone();
        let mut r2 = Vec::new();
        let grid = parse_tsv("x\t1\ny\t2\n");
        assert_eq!(
            grid,
            vec![vec!["x".to_string(), "1".to_string()], vec!["y".to_string(), "2".to_string()]]
        );
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let id = || format!("n{}", seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        assert!(apply_paste_grid(&mut f2, &mut r2, 0, 0, &grid, id));
        assert_eq!(r2.len(), 2);
        assert_eq!(r2[0].values["f1"], serde_json::json!("x"));
        assert_eq!(r2[0].values["f2"], serde_json::json!(1));
        // 全空网格贴到空行：无变化 + 不补行
        let mut r3: Vec<TableRow> = Vec::new();
        let mut f3 = fields.clone();
        assert!(!apply_paste_grid(&mut f3, &mut r3, 0, 0, &parse_tsv("\t\n"), id));
        assert!(r3.is_empty());
        // singleSelect 非选项跳过
        let mut fs = vec![field("s", "选", "singleSelect")];
        fs[0].options = Some(vec!["甲".into()]);
        let mut rs: Vec<TableRow> = Vec::new();
        assert!(!apply_paste_grid(&mut fs, &mut rs, 0, 0, &parse_tsv("乙"), id));
        assert!(rs.is_empty());
        assert!(apply_paste_grid(&mut fs, &mut rs, 0, 0, &parse_tsv("甲"), id));
        assert_eq!(rs[0].values["s"], serde_json::json!("甲"));
    }

    #[test]
    fn paste_expands_missing_columns_and_skips_image() {
        let mut fields = vec![field("f1", "a", "text")];
        let mut rows: Vec<TableRow> = vec![];
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let id = || format!("n{}", seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let grid = parse_tsv("v\t2");
        assert!(apply_paste_grid(&mut fields, &mut rows, 0, 0, &grid, id));
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1].name, "字段2");
        assert_eq!(fields[1].field_type, "text");
        // 补列是 text 字段：数值原样为字符串
        assert_eq!(rows[0].values[&fields[1].id], serde_json::json!("2"));
    }

    #[test]
    fn image_value_normalize_and_equality() {
        let legacy = serde_json::json!(["a.png", "b.png"]);
        let norm = normalize_image_value(&legacy);
        assert_eq!(image_entries(&norm), vec!["a.png", "b.png"]);
        // 空图 ≈ 缺键
        assert!(cell_value_equal(Some(&serde_json::json!({"images": []})), None));
        assert!(!cell_value_equal(Some(&serde_json::json!({"images": ["a.png"]})), None));
        // display 差异视为变化
        assert!(!cell_value_equal(
            Some(&serde_json::json!({"images": ["a.png"]})),
            Some(&serde_json::json!({"images": ["a.png"], "display": "grid"}))
        ));
        // 脏条目剔除
        assert_eq!(image_entries(&normalize_image_value(&serde_json::json!(["a.png", 3]))), vec!["a.png"]);
    }

    #[test]
    fn style_patch_summary_and_application() {
        let mut t = table(vec![field("f1", "a", "text")], vec![row("r1", vec![]), row("r2", vec![])]);
        let region = Region { row_start: 0, row_end: 1, col_start: 0, col_end: 0 };
        let patch = StylePatch {
            b: KeyOp::Set(Some(true)),
            color: KeyOp::Set(Some("#e05252".into())),
            ..Default::default()
        };
        assert!(apply_style_patch(&t.fields, &mut t.rows, &region, &patch));
        assert!(style_of(&t.rows[0], "f1").b);
        assert_eq!(style_of(&t.rows[0], "f1").color.as_deref(), Some("#e05252"));
        // 同补丁再应用 = 无变化
        assert!(!apply_style_patch(&t.fields, &mut t.rows, &region, &patch));
        // summary 全开
        let sum = style_summary(&t.fields, &t.rows, &region);
        assert_eq!(sum.bold, OptState::Value(Some(true)));
        assert_eq!(sum.italic, OptState::Value(Some(false)));
        // 清除
        let clear = StylePatch { b: KeyOp::Set(None), color: KeyOp::Set(None), ..Default::default() };
        assert!(apply_style_patch(&t.fields, &mut t.rows, &region, &clear));
        assert!(t.rows[0].styles.is_none(), "全默认样式应删键");
    }

    /// 字体值承载系统已安装族名（可含空格）：补丁 → 落盘 JSON → 回读 → 汇总 全链一致，清除后随全默认删键。
    #[test]
    fn 字体族名值随样式落盘往返() {
        let mut t = table(vec![field("f1", "a", "text")], vec![row("r1", vec![])]);
        let region = Region { row_start: 0, row_end: 0, col_start: 0, col_end: 0 };
        let patch = StylePatch { font: KeyOp::Set(Some("Microsoft YaHei".into())), ..Default::default() };
        assert!(apply_style_patch(&t.fields, &mut t.rows, &region, &patch));
        assert_eq!(t.rows[0].styles.as_ref().unwrap()["f1"]["font"], Value::String("Microsoft YaHei".into()));
        let mut back: TableFile = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(style_of(&back.rows[0], "f1").font.as_deref(), Some("Microsoft YaHei"));
        assert_eq!(
            style_summary(&back.fields, &back.rows, &region).font,
            OptState::Value(Some("Microsoft YaHei".into()))
        );
        let clear = StylePatch { font: KeyOp::Set(None), ..Default::default() };
        assert!(apply_style_patch(&back.fields, &mut back.rows, &region, &clear));
        assert!(back.rows[0].styles.is_none(), "清除字体后全默认样式应删键");
    }

    #[test]
    fn row_ops_move_and_duplicate() {
        let mut t = table(vec![field("f1", "a", "text")], vec![row("r1", vec![]), row("r2", vec![]), row("r3", vec![])]);
        assert!(move_row(&mut t, "r3", 0));
        assert_eq!(t.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["r3", "r1", "r2"]);
        // 相等 no-op
        assert!(!move_row(&mut t, "r3", 0));
        // 末行拖到表尾 = 原位 no-op
        assert!(!move_row(&mut t, "r2", 3));
        assert!(duplicate_row(&mut t, "r3", "r4".into()));
        assert_eq!(t.rows[1].id, "r4");
        // remove_field 同步清值与样式
        t.rows[0].values.insert("f1".into(), serde_json::json!("x"));
        remove_field(&mut t, "f1");
        assert!(t.fields.is_empty());
        assert!(t.rows[0].values.is_empty());
    }

    #[test]
    fn retype_clears_incompatible_calc() {
        let mut t = table(vec![field("f1", "a", "number")], vec![]);
        t.fields[0].calc_type = Some("sum".into());
        retype_field(&mut t, "f1", "text");
        assert_eq!(t.fields[0].field_type, "text");
        assert_eq!(t.fields[0].calc_type, None);
        assert!(!retype_field(&mut t, "f1", "text"));
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut t = table(vec![field("f1", "a", "text")], vec![row("r1", vec![("f1", serde_json::json!("x"))])]);
        let snap: TableSnapshot = (t.fields.clone(), t.rows.clone());
        add_row(&mut t, "r2".into());
        assert_eq!(t.rows.len(), 2);
        restore(&mut t, snap);
        assert_eq!(t.rows.len(), 1);
    }
}
