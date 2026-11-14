//! 路径与命名基元：重名顺延与改名后路径跟随的唯一口径。

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
    fn under_requires_segment_boundary() {
        assert!(under("a/b.md", "a"));
        assert!(under("a", "a"));
        assert!(!under("ab.md", "a"));
        assert!(!under("a", "a/b"));
    }
}
