//! 笔记正文三方合并（纯函数，不依赖 CRDT）：以最近落盘文本为共同祖先，
//! 把两侧差异都保留；冲突粒度为单个差异块（hunk 原子整体），重叠时按指定侧取舍。

/// 单个差异块：`base` 的 `[at, at + remove)` 字节区间替换为 `insert`（`remove = 0` 为纯插入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHunk {
    pub at: usize,
    pub remove: usize,
    pub insert: String,
}

/// LCS 表规模上限：两侧中段各 ≤ 此值才走精确差分。超出时先按「两侧各出现且仅出现一次」的
/// 公共行切段逐段差分；连锚点都没有才降级为单 hunk 整段替换（那一段的合并粒度变粗，
/// 对侧落在其中的改动按裁决方向取舍）。
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
    hunks_from_tokens_at(a, b, 0)
}

/// 长中段的分段锚点：两侧各出现一次、且在两侧的出现顺序一致的行。
/// 锚点行是两侧都同意的公共文本，不含任何一侧的改动，切开后各段互不影响。
fn unique_anchors(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;
    let mut b_index: HashMap<&str, usize> = HashMap::with_capacity(b.len());
    let mut b_dup: Vec<&str> = Vec::new();
    for (i, token) in b.iter().enumerate() {
        if b_index.insert(token, i).is_some() {
            b_dup.push(token);
        }
    }
    for token in b_dup {
        b_index.remove(token);
    }
    let mut seen: HashMap<&str, Option<usize>> = HashMap::with_capacity(a.len());
    for (i, token) in a.iter().enumerate() {
        seen.entry(token).and_modify(|slot| *slot = None).or_insert(Some(i));
    }
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, token) in a.iter().enumerate() {
        let Some(Some(_)) = seen.get(token) else { continue };
        let Some(&j) = b_index.get(token) else { continue };
        // 顺序必须一致：倒序锚点切出交叉段，逐段差分反而错位
        if out.last().is_some_and(|(_, last_j)| *last_j >= j) {
            continue;
        }
        out.push((i, j));
    }
    out
}

/// `a[0]` 位于原文的 `base_at` 字节处时求差异块（分段差分用）。
fn hunks_from_tokens_at(a: &[&str], b: &[&str], base_at: usize) -> Vec<TextHunk> {
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
    let at = base_at + a_offsets[head];
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
        // 长中段：按公共锚点行切段，各段独立差分（两侧改动落在不同段即互不影响）；
        // 无锚点才退化为整段替换——那一段里对侧的改动会被整体判为冲突
        let anchors = unique_anchors(a_mid, b_mid);
        if anchors.is_empty() {
            return vec![TextHunk { at, remove: a_mid.concat().len(), insert: b_mid.concat() }];
        }
        let mut out: Vec<TextHunk> = Vec::new();
        let mut a_from = 0usize;
        let mut b_from = 0usize;
        let mut pos = at;
        for (a_idx, b_idx) in anchors {
            // 段内未改动的行不会产生 hunk，字节位置要由切段本身推进：先段、再锚点行
            out.extend(hunks_from_tokens_at(&a_mid[a_from..a_idx], &b_mid[b_from..b_idx], pos));
            pos += a_mid[a_from..a_idx].iter().map(|token| token.len()).sum::<usize>();
            pos += a_mid[a_idx].len();
            a_from = a_idx + 1;
            b_from = b_idx + 1;
        }
        out.extend(hunks_from_tokens_at(&a_mid[a_from..], &b_mid[b_from..], pos));
        return out;
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

/// 两条同锚点纯插入是否共享行：共享行时按并集拼接会把同一行写成两遍（静默重复内容），
/// 这种情形交回冲突更安全（调用方拒绝写入，本端内容仍在）。
fn inserts_share_lines(a: &str, b: &str) -> bool {
    let left: std::collections::HashSet<&str> = tokenize(a).into_iter().collect();
    tokenize(b).into_iter().any(|line| left.contains(line))
}

/// 保存落盘决策（本地写盘与空间整文上行同一条策略）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavePlan {
    /// 磁盘未变：直接写本端全文。
    Write(String),
    /// 两侧改到同一处：拒绝写入（调用方提示重试，本端内容不动）。
    Conflict,
    /// 磁盘已被改动但与本次不重叠：写三方合流结果（本端优先）。
    Merge(String),
}

/// 按「共同祖先 / 本端全文 / 磁盘现状」决定这一笔写什么。
pub fn plan_save(base: &str, content: &str, current: &str) -> SavePlan {
    if current == base {
        return SavePlan::Write(content.to_owned());
    }
    if merge3_conflict(base, content, current) {
        return SavePlan::Conflict;
    }
    SavePlan::Merge(merge3(base, content, current, ConflictSide::Ours))
}

/// 两侧 hunk 是否落在同一区间：纯插入只在锚点严格落在对方区间内部时才算冲突。
fn hunks_overlap(a: &TextHunk, b: &TextHunk) -> bool {
    if a.remove > 0 && b.remove > 0 {
        return a.at < b.at + b.remove && b.at < a.at + a.remove;
    }
    // 同一锚点上的纯插入：内容互不相同且不共享行才按序拼接（内容都保留）；
    // 完全相同的插入由 merge3 只落一份，共享行的插入判冲突（拼接会重复）
    if a.remove == 0 && b.remove == 0 {
        return a.at == b.at && a.insert != b.insert && inserts_share_lines(&a.insert, &b.insert);
    }
    let (insert, remove) = if a.remove == 0 { (a, b) } else { (b, a) };
    // 落在对方区间端点上的插入不冲突：先插后删（顺序分支的「同起点先插」）即两侧内容都保留
    insert.at > remove.at && insert.at < remove.at + remove.remove
}

/// 三方合流是否存在重叠改动：重叠即无法安全对齐（调用方应拒绝写入而不是选边）。
/// 两侧完全相同的改动不算冲突（合并时只落一份）。
pub fn merge3_conflict(base: &str, ours: &str, theirs: &str) -> bool {
    if ours == theirs || base == ours || base == theirs {
        return false;
    }
    let our_hunks = diff_hunks(base, ours);
    let their_hunks = diff_hunks(base, theirs);
    our_hunks
        .iter()
        .any(|a| their_hunks.iter().any(|b| a != b && hunks_overlap(a, b)))
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
            // 两侧完全相同的改动只落一份：同锚点纯插入已不算区间冲突，不能再靠冲突分支去重
            if oh.at == th.at && oh.remove == th.remove && oh.insert == th.insert {
                out.push_str(&base[pos..oh.at]);
                out.push_str(&oh.insert);
                pos = oh.at + oh.remove;
                i += 1;
                j += 1;
                continue;
            }
            if hunks_overlap(oh, th) {
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
    fn 冲突判定只看重叠改动() {
        let base = "a\nb\nc\n";
        // 不同行各自改动：无冲突
        assert!(!merge3_conflict(base, "A\nb\nc\n", "a\nb\nC\n"));
        // 同一行两侧都改：冲突
        assert!(merge3_conflict(base, "a\nX\nc\n", "a\nY\nc\n"));
        // 两侧相同改动：不算冲突（合并只落一份）
        assert!(!merge3_conflict(base, "a\nX\nc\n", "a\nX\nc\n"));
        // 单侧未改：无冲突
        assert!(!merge3_conflict(base, "a\nb\nc\nd\n", base));
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
    fn 同锚点双插入两侧都保留() {
        // 两侧在同一行后各插一行：不算冲突，按「本端在前」拼接（内容都不丢）
        let base = "a
b
";
        let ours = "a
OURS
b
";
        let theirs = "a
THEIRS
b
";
        assert!(!merge3_conflict(base, ours, theirs));
        assert_eq!(merge3(base, ours, theirs, ConflictSide::Theirs), "a
OURS
THEIRS
b
");
        assert_eq!(merge3(base, ours, theirs, ConflictSide::Ours), "a
OURS
THEIRS
b
");
    }

    /// 保存决策：磁盘未变直写、外部改动不重叠时合流、重叠时交冲突由调用方拒绝。
    #[test]
    fn save_plan_covers_unchanged_merged_and_conflict() {
        let base = "a\nb\nc\n";
        assert_eq!(plan_save(base, "a\nB\nc\n", base), SavePlan::Write("a\nB\nc\n".into()));
        let external = "A\nb\nc\n";
        assert_eq!(
            plan_save(base, "a\nB\nc\n", external),
            SavePlan::Merge("A\nB\nc\n".into())
        );
        assert_eq!(plan_save(base, "a\nX\nc\n", "a\nY\nc\n"), SavePlan::Conflict);
    }

    #[test]
    fn 同锚点插入共享行判冲突() {
        // 两侧同一行后各插入一段，但两段都含 "X"：按并集拼会写出两个 X，判冲突交调用方拒绝
        let base = "a\nb\n";
        let ours = "a\nX\nY\nb\n";
        let theirs = "a\nX\nb\n";
        assert!(merge3_conflict(base, ours, theirs));
        // 不共享行仍按序拼接（两侧内容都保留）
        assert!(!merge3_conflict(base, "a\nP\nb\n", "a\nQ\nb\n"));
    }

    #[test]
    fn 同锚点相同插入只落一份() {
        // 两侧同一行后各插同一行、别处再各改一行：相同插入不能叠成两行，两处改动都要保留
        let base = "a
b
c
";
        let ours = "a
X
b
O
c
";
        let theirs = "a
X
b
T
c
";
        let merged = merge3(base, ours, theirs, ConflictSide::Ours);
        assert_eq!(merged, "a
X
b
O
T
c
");
    }

    #[test]
    fn 长中段按公共行切段后两侧改动都保留() {
        // 2000 行、首尾无公共前后缀：中段超出 LCS 上限，靠公共行切段才能保住两侧改动
        let base: String = (0..2000).map(|i| format!("l{i}
")).collect();
        let ours = base.replacen("l0
", "o0
", 1).replacen("l10
", "ours10
", 1);
        let theirs = base.replacen("l0
", "t0
", 1).replacen("l1990
", "theirs1990
", 1);
        let merged = merge3(&base, &ours, &theirs, ConflictSide::Theirs);
        let mut expected = String::new();
        for i in 0..2000 {
            match i {
                0 => expected.push_str("t0
"),
                10 => expected.push_str("ours10
"),
                1990 => expected.push_str("theirs1990
"),
                other => expected.push_str(&format!("l{other}
")),
            }
        }
        assert_eq!(merged, expected);
    }

    #[test]
    fn 长中段两侧改动不重叠不判冲突() {
        // 首末行两侧都未改（只改中段两处）：整体降级会因区间重叠误判冲突，切段后各行其段
        let base: String = (0..2000).map(|i| format!("l{i}
")).collect();
        let ours = base.replacen("l10
", "ours10
", 1);
        let theirs = base.replacen("l1990
", "theirs1990
", 1);
        assert!(!merge3_conflict(&base, &ours, &theirs));
        let merged = merge3(&base, &ours, &theirs, ConflictSide::Theirs);
        assert_eq!(merged, ours.replacen("l1990
", "theirs1990
", 1));
    }

    #[test]
    fn 超限中段无公共行时降级单hunk不丢内容() {
        let base = "x\n".repeat(2000);
        let ours: String = "y\n".repeat(2000);
        let theirs = format!("{base}tail\n");
        let merged = merge3(&base, &ours, &theirs, ConflictSide::Theirs);
        assert_eq!(merged, format!("{ours}tail\n"));
    }
}
