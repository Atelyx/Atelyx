//! 路径与命名基元：重名顺延、改名后路径跟随与相对路径归一化的唯一口径。

/// 重名顺延：stem.ext 已被占用时依次尝试 stem-2.ext、stem-3.ext…，返回首个未占用的名字。
///
/// taken 由调用方给出（磁盘、内存名表、服务端清单都能用）；ext 带点，无扩展名传空串。
/// 后缀统一为「-数字」：同一种重名在不同入口给出不同写法（ (n) / -n / 空格+数字）会让用户
/// 以为不是同一套规则。
pub fn unique_stem(stem: &str, ext: &str, taken: impl Fn(&str) -> bool) -> String {
    let first = format!("{stem}{ext}");
    if !taken(&first) {
        return first;
    }
    for n in 2u32.. {
        let candidate = format!("{stem}-{n}{ext}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("候选名字空间有限，序号递增必收敛")
}

/// 同 [unique_stem]，无扩展名。
pub fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    unique_stem(base, "", taken)
}

/// 仓库相对路径的规范形态：`\` 与 `/` 同作分隔符，退掉 `.` 段与重复分隔符，返回以 `/`
/// 分隔的净化路径；拒绝空路径 / 绝对路径 / `..` 段 / 盘符前缀。纯 `.` 归一为空串（= 仓库根），
/// 要指向具体条目用 [normalize_item]。
///
/// 内容路径在 Windows 与 Linux 客户端之间共享，两端口径必须一致：盘符前缀在 Unix 上只是普通
/// 文件名，落进共享内容后 Windows 客户端会解析成绝对路径。
pub fn normalize(rel: &str) -> Result<String, String> {
    if rel.is_empty() {
        return Err("非法路径：空路径".to_string());
    }
    let text = rel.replace('\\', "/");
    if text.starts_with('/') {
        return Err(format!("非法路径：绝对路径 ({rel})"));
    }
    let mut segs: Vec<&str> = Vec::new();
    for seg in text.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(format!("非法路径：含越界段 ({rel})")),
            s if is_drive_prefix(s) => return Err(format!("非法路径：盘符前缀 ({rel})")),
            s => segs.push(s),
        }
    }
    Ok(segs.join("/"))
}

/// 同 [normalize]，但归一结果必须指向具体条目（`.` / `./` 这类空结果判为非法）。
pub fn normalize_item(rel: &str) -> Result<String, String> {
    let text = normalize(rel)?;
    if text.is_empty() {
        return Err(format!("非法路径：空路径 ({rel})"));
    }
    Ok(text)
}

/// 段是否为盘符形态（`C:` / `C:x.md`）：Windows 上非法或绝对，Unix 上只是普通名字。
fn is_drive_prefix(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// `rel` 等于 `base` 或位于其子树内（`base` 为规范仓库相对路径，不带首尾斜杠）。
pub fn under(rel: &str, base: &str) -> bool {
    rel == base || rel.strip_prefix(base).is_some_and(|rest| rest.starts_with('/'))
}

/// 改名 / 移动后的路径跟随：`rel` 等于 `from` 或在其子树内 → 换成 `to` 下的对应路径，
/// 否则 `None`。`from` / `to` 为规范仓库相对路径（不带首尾斜杠）。
pub fn remap_prefix(rel: &str, from: &str, to: &str) -> Option<String> {
    if rel == from {
        return Some(to.to_string());
    }
    let rest = rel.strip_prefix(from)?.strip_prefix('/')?;
    Some(format!("{to}/{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn returns_first_free_suffix() {
        // 首个候选可用：原名不动
        assert_eq!(unique_stem("笔记", ".md", |_| false), "笔记.md");
        // 依次跳过已占用的候选
        let taken = ["笔记.md", "笔记-2.md"];
        assert_eq!(unique_stem("笔记", ".md", |n| taken.contains(&n)), "笔记-3.md");
        // 无扩展名同样口径
        let labels = ["对话", "对话-2"];
        assert_eq!(unique_name("对话", |n| labels.contains(&n)), "对话-3");
    }
    #[test]
    fn remap_follows_exact_and_subtree_only() {
        assert_eq!(remap_prefix("a/b.md", "a/b.md", "c/d.md").as_deref(), Some("c/d.md"));
        assert_eq!(remap_prefix("a/b/c.md", "a/b", "x/y").as_deref(), Some("x/y/c.md"));
        // 前缀相同但不是子树（a/bc.md 不在 a/b 下）
        assert_eq!(remap_prefix("a/bc.md", "a/b", "x"), None);
        assert_eq!(remap_prefix("other.md", "a/b", "x"), None);
        assert_eq!(remap_prefix("a/b", "a", "z").as_deref(), Some("z/b"));
    }

    #[test]
    fn normalize_folds_separators_and_rejects_escapes() {
        // 尾斜杠 / `./` 前缀 / 反斜杠 / 中间双斜杠 / 中间 `.` 段 → 同一规范形态
        assert_eq!(normalize("notes/").as_deref(), Ok("notes"));
        assert_eq!(normalize("./notes").as_deref(), Ok("notes"));
        assert_eq!(normalize("a//b").as_deref(), Ok("a/b"));
        assert_eq!(normalize("a\\b").as_deref(), Ok("a/b"));
        assert_eq!(normalize("a/./b/").as_deref(), Ok("a/b"));
        // 纯 `.` = 仓库根（空串），不是具体条目
        assert_eq!(normalize(".").as_deref(), Ok(""));
        for bad in ["", "/abs", "../x", "a/../b", "C:x.md"] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn normalize_item_rejects_root_forms() {
        assert_eq!(normalize_item("a/b.md").as_deref(), Ok("a/b.md"));
        assert!(normalize_item(".").is_err());
        assert!(normalize_item("./").is_err());
    }

    #[test]
    fn under_requires_segment_boundary() {
        assert!(under("a/b.md", "a"));
        assert!(under("a", "a"));
        assert!(!under("ab.md", "a"));
        assert!(!under("a", "a/b"));
    }
}
