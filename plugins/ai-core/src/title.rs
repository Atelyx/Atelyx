//! 话题标题清洗：去引号 / 「标题：」前缀 / 编号 / 取首行 / 截 16 字。

pub fn clean_title(raw: &str) -> String {
    let first_line = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut t = first_line.trim();
    for prefix in ["标题：", "标题:"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim();
        }
    }
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '、' || c == ' ');
    let t = t.trim_matches(|c: char| {
        matches!(c, '"' | '“' | '”' | '「' | '」' | '『' | '』' | '\'' | '‘' | '’' | '　')
    });
    t.chars().take(16).collect()
}
