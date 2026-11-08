//! 笔记正文三方合并（纯函数，不依赖 CRDT）：以最近落盘文本为共同祖先，
//! 把两侧差异都保留；冲突粒度为单个差异块（hunk 原子整体），重叠时按指定侧取舍。

/// 单个差异块：`base` 的 `[at, at + remove)` 字节区间替换为 `insert`（`remove = 0` 为纯插入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHunk {
    pub at: usize,
    pub remove: usize,
    pub insert: String,
}

/// LCS 表规模上限：两侧中段各 ≤ 此值才走精确差分；超出降级为单 hunk 整段替换——
/// 对侧落在该区间内的改动会与本端整体替换判为冲突、按裁决方向取舍（内容不静默丢失，但合并粒度变粗）。
const MAX_DIFF_TOKENS: usize = 1200;

/// 差异取向：重叠冲突时保留哪一侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
}

/// 按「含行尾换行的行」切 token：拼接 token 恒等于原文（末行可无换行）。
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            tokens.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

/// 各 token 起始字节偏移（末位为全文长度）。
fn token_offsets(tokens: &[&str]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(tokens.len() + 1);
    offsets.push(0usize);
    let mut acc = 0usize;
    for t in tokens {
        acc += t.len();
        offsets.push(acc);
    }
    offsets
}

/// 求 `a → b` 的差异块：裁掉公共前后缀后对中段做 LCS，逐段合并为 hunk。
fn hunks_from_tokens(a: &[&str], b: &[&str]) -> Vec<TextHunk> {
    let a_offsets = token_offsets(a);
    let mut head = 0usize;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0usize;
    while tail < a.len() - head && tail < b.len() - head && a[a.len() - 1 - tail] == b[b.len() - 1 - tail] {
        tail += 1;
    }
    let a_mid = &a[head..a.len() - tail];
    let b_mid = &b[head..b.len() - tail];
    let at = a_offsets[head];
    if a_mid.is_empty() && b_mid.is_empty() {
        return Vec::new();
    }
    if a_mid.is_empty() {
        return vec![TextHunk { at, remove: 0, insert: b_mid.concat() }];
    }
    if b_mid.is_empty() {
        return vec![TextHunk { at, remove: a_mid.concat().len(), insert: String::new() }];
    }
    if a_mid.len() > MAX_DIFF_TOKENS || b_mid.len() > MAX_DIFF_TOKENS {
        return vec![TextHunk { at, remove: a_mid.concat().len(), insert: b_mid.concat() }];
    }

    let n = a_mid.len();
    let m = b_mid.len();
    // table[i][j] = aMid[i..] 与 bMid[j..] 的 LCS 长度（≤ MAX_DIFF_TOKENS，u16 足够）
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a_mid[i] == b_mid[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut hunks: Vec<TextHunk> = Vec::new();
    let mut hunk: Option<TextHunk> = None;
    let mut pos = at;
    let mut i = 0usize;
    let mut j = 0usize;
    while i < n || j < m {
        if i < n && j < m && a_mid[i] == b_mid[j] {
            if let Some(h) = hunk.take() {
                hunks.push(h);
            }
            pos += a_mid[i].len();
            i += 1;
            j += 1;
            continue;
        }
        // 与 LCS 表一致的回溯方向：先插后删（相等时优先插入，保证 hunk 内含替换语义）
        if j < m && (i >= n || table[i][j + 1] >= table[i + 1][j]) {
            let h = hunk.get_or_insert_with(|| TextHunk { at: pos, remove: 0, insert: String::new() });
            h.insert.push_str(b_mid[j]);
            j += 1;
            continue;
        }
        let h = hunk.get_or_insert_with(|| TextHunk { at: pos, remove: 0, insert: String::new() });
        h.remove += a_mid[i].len();
        pos += a_mid[i].len();
        i += 1;
    }
    if let Some(h) = hunk {
        hunks.push(h);
    }
    hunks
}

/// 求 `base → next` 的差异块（相等返回空表）。
pub fn diff_hunks(base: &str, next: &str) -> Vec<TextHunk> {
    if base == next {
        return Vec::new();
    }
    hunks_from_tokens(&tokenize(base), &tokenize(next))
}

/// 两侧 hunk 是否落在同一区间：纯插入只在锚点严格落在对方区间内部时才算冲突。
fn hunks_overlap(a: &TextHunk, b: &TextHunk) -> bool {
    if a.remove > 0 && b.remove > 0 {
        return a.at < b.at + b.remove && b.at < a.at + a.remove;
    }
    if a.remove == 0 && b.remove == 0 {
        return a.at == b.at;
    }
    let (insert, remove) = if a.remove == 0 { (a, b) } else { (b, a) };
    // 落在对方区间端点上的插入不冲突：先插后删（顺序分支的「同起点先插」）即两侧内容都保留
    insert.at > remove.at && insert.at < remove.at + remove.remove
}

/// 三方合并：以 `base` 为共同祖先，把 `ours` / `theirs` 的差异都保留。
/// 重叠冲突按 `side` 取一侧、另一侧与之重叠的 hunk 一并消费——结果只由入参决定。
pub fn merge3(base: &str, ours: &str, theirs: &str, side: ConflictSide) -> String {
    if ours == theirs {
        return ours.to_owned();
    }
    if base == ours {
        return theirs.to_owned();
    }
    if base == theirs {
        return ours.to_owned();
    }
    let our_hunks = diff_hunks(base, ours);
    let their_hunks = diff_hunks(base, theirs);

    let mut out = String::new();
    let mut pos = 0usize;
    let mut i = 0usize;
    let mut j = 0usize;
    // hunk 表内区间有序且互不相交；跨侧同区间已按冲突规则消费完毕，
    // 故每次取用的 hunk 起点恒 ≥ pos（不会回退覆盖已写入区间）
    while i < our_hunks.len() || j < their_hunks.len() {
        let ours_hunk = our_hunks.get(i);
        let theirs_hunk = their_hunks.get(j);
        if let (Some(oh), Some(th)) = (ours_hunk, theirs_hunk) {
            if hunks_overlap(oh, th) {
                if oh.at == th.at && oh.remove == th.remove && oh.insert == th.insert {
                    out.push_str(&base[pos..oh.at]);
                    out.push_str(&oh.insert);
                    pos = oh.at + oh.remove;
                    i += 1;
                    j += 1;
                    continue;
                }
                // 同区间冲突只取指定一侧，另一侧与之重叠的 hunk 一并消费
                match side {
                    ConflictSide::Ours => {
                        out.push_str(&base[pos..oh.at]);
                        out.push_str(&oh.insert);
                        pos = oh.at + oh.remove;
                        i += 1;
                        while let Some(nh) = their_hunks.get(j) {
                            if !hunks_overlap(nh, oh) {
                                break;
                            }
                            j += 1;
                        }
                    }
                    ConflictSide::Theirs => {
                        out.push_str(&base[pos..th.at]);
                        out.push_str(&th.insert);
                        pos = th.at + th.remove;
                        j += 1;
                        while let Some(nh) = our_hunks.get(i) {
                            if !hunks_overlap(nh, th) {
                                break;
                            }
                            i += 1;
                        }
                    }
                }
                continue;
            }
        }
        // 同起点时先应用纯插入（插入锚点在对方区间端点，先插后删才不丢插入内容）
        let pick_ours = match (ours_hunk, theirs_hunk) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(oh), Some(th)) => {
                oh.at < th.at || (oh.at == th.at && oh.remove == 0)
            }
        };
        let next = if pick_ours { ours_hunk.expect("pick_ours 时 ours_hunk 必存在") } else { theirs_hunk.expect("否则 theirs_hunk 必存在") };
        out.push_str(&base[pos..next.at]);
        out.push_str(&next.insert);
        pos = next.at + next.remove;
        if pick_ours {
            i += 1;
        } else {
            j += 1;
        }
    }
    out.push_str(&base[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 差分与合并的基本还原() {
        assert!(diff_hunks("abc", "abc").is_empty());
        // 无换行文本为单 token：差异按 token 粒度整段替换
        assert_eq!(
            diff_hunks("abc", "abXc"),
            vec![TextHunk { at: 0, remove: 3, insert: "abXc".into() }]
        );
        assert_eq!(
            diff_hunks("a\nb\nc\n", "a\nX\nc\n"),
            vec![TextHunk { at: 2, remove: 2, insert: "X\n".into() }]
        );
        // 拼接 token 恒等于原文
        assert_eq!(tokenize("a\r\nb\nc").concat(), "a\r\nb\nc");
        assert_eq!(tokenize("a\n\n").concat(), "a\n\n");
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn 非重叠改动两侧保留() {
        let base = "line1\nline2\nline3\n";
        let ours = base.replace("line1", "ours1");
        let theirs = base.replace("line3", "theirs3");
        let merged = merge3(base, &ours, &theirs, ConflictSide::Theirs);
        assert_eq!(merged, "ours1\nline2\ntheirs3\n");
        // 换侧调用对非重叠部分结果一致
        assert_eq!(merge3(base, &ours, &theirs, ConflictSide::Ours), merged);
    }

    #[test]
    fn 单侧未改动直接取另一侧() {
        assert_eq!(merge3("base", "base", "theirs", ConflictSide::Ours), "theirs");
        assert_eq!(merge3("base", "ours", "base", ConflictSide::Theirs), "ours");
        assert_eq!(merge3("base", "same", "same", ConflictSide::Ours), "same");
    }

    #[test]
    fn 同区间冲突按侧取舍() {
        let base = "aa\nbb\ncc\n";
        let ours = base.replace("bb", "ours-bb");
        let theirs = base.replace("bb", "theirs-bb");
        assert_eq!(merge3(base, &ours, &theirs, ConflictSide::Theirs), "aa\ntheirs-bb\ncc\n");
        assert_eq!(merge3(base, &ours, &theirs, ConflictSide::Ours), "aa\nours-bb\ncc\n");
    }

    #[test]
    fn 同起点插入与删除并存时两侧都保留() {
        // ours 纯插入在行首锚点，theirs 删除该行：锚点在删除区间端点上，不冲突
        let base = "keep\nvinyl\nkeep2\n";
        let ours = base.replace("keep\n", "keep\ninserted\n");
        let theirs = base.replace("vinyl\n", "");
        let merged = merge3(base, &ours, &theirs, ConflictSide::Theirs);
        assert_eq!(merged, "keep\ninserted\nkeep2\n");
    }

    #[test]
    fn 完全相同改动幂等() {
        let base = "a\nb\n";
        let next = "a\nb\nc\n";
        let ours = next;
        let theirs = base.replace("b\n", "b\nc\n");
        assert_eq!(merge3(base, ours, &theirs, ConflictSide::Theirs), next);
    }

    #[test]
    fn 中文与多字节文本按字节偏移合并正确() {
        let base = "第一行\n第二行\n第三行\n";
        let ours = base.replace("第二行", "第二行（本端改）");
        let theirs = base.replace("第三行", "第三行（对端改）");
        let merged = merge3(base, &ours, &theirs, ConflictSide::Theirs);
        assert_eq!(merged, "第一行\n第二行（本端改）\n第三行（对端改）\n");
    }

    #[test]
    fn 超限中段降级单hunk不丢内容() {
        let base = "x\n".repeat(2000);
        let ours: String = "y\n".repeat(2000);
        let theirs = format!("{base}tail\n");
        let merged = merge3(&base, &ours, &theirs, ConflictSide::Theirs);
        assert_eq!(merged, format!("{ours}tail\n"));
    }
}
