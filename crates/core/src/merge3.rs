//! 笔记正文三方合并（纯函数，不依赖 CRDT）：以最近落盘文本为共同祖先，
//! 把两侧差异都保留；冲突粒度为单个差异块（hunk 原子整体），重叠时按指定侧取舍。
//! 差异块先按行定位改动，再在块内按字符细化，同一行里互不相邻的改动可合流。

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

/// 段落切段的最短段长：比这更短的段不再单独切出，免得碎段让切段本身成为开销。
const MIN_PARAGRAPH_TOKENS: usize = 8;

/// 空行分段点：两侧按同一序号出现的空行（`unique_anchors` 要求行在全篇唯一，正文以重复行
/// 为主时一个锚点都拿不到，而段落边界仍能把两侧改动隔开）。段长不足 / 两侧空行数不等
/// 时多出的空行不切，交给段内差分。
fn blank_line_anchors(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let min_segment = MIN_PARAGRAPH_TOKENS;
    let blanks = |tokens: &[&str]| -> Vec<usize> {
        tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| **token == "\n")
            .map(|(i, _)| i)
            .collect()
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut segment_start = 0usize;
    for (i, j) in blanks(a).into_iter().zip(blanks(b)) {
        if i + 1 - segment_start < min_segment {
            continue;
        }
        out.push((i, j));
        segment_start = i + 1;
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
        // 唯一行锚点一个都没有时再退到空行段落边界，仍切不出段才整段替换——
        // 整段替换会把对侧落在同段的改动一并判成冲突（两个 hunk 塌成一个）
        let mut anchors = unique_anchors(a_mid, b_mid);
        if anchors.is_empty() {
            anchors = blank_line_anchors(a_mid, b_mid);
        }
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

/// 字符级细化上限：改动块与替换文本都在此长度内才在块内再按字符求差；超出保持整块替换
/// （字符 LCS 表代价 O(n*m)，上限内约 1200² 格）。
const MAX_REFINE_CHARS: usize = 1200;

/// 按单字符切 token（`token_offsets` 累加仍得原字节偏移，多字节字符也落在字符边界）。
fn tokenize_chars(text: &str) -> Vec<&str> {
    text.char_indices()
        .map(|(i, c)| &text[i..i + c.len_utf8()])
        .collect()
}

/// 行级 hunk 细化到字符级：行级差分只按整行定位改动，同一行里互不相邻的两处改动会并成
/// 一个替换块——两侧各改一处就被判成区间重叠，协作笔记里同改一段必然拒写。块内再按字符
/// 求差后两处改动各自成块，不相邻即可合流；长块保持整块只让粒度变粗，不改坏结果。
fn refine_hunks(base: &str, hunks: Vec<TextHunk>) -> Vec<TextHunk> {
    let mut out = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        let region = &base[hunk.at..hunk.at + hunk.remove];
        // 纯插入 / 纯删除不会被字符差分再拆开，只有替换块需要细化
        if hunk.remove == 0
            || hunk.insert.is_empty()
            || region.chars().count() > MAX_REFINE_CHARS
            || hunk.insert.chars().count() > MAX_REFINE_CHARS
        {
            out.push(hunk);
            continue;
        }
        let refined =
            hunks_from_tokens_at(&tokenize_chars(region), &tokenize_chars(&hunk.insert), hunk.at);
        if refined.is_empty() {
            // 非空改动块恒产出至少一块；空结果只可能来自不变量被破坏，退回整块不丢内容
            out.push(hunk);
        } else {
            out.extend(refined);
        }
    }
    out
}

/// 求 `base → next` 的差异块（相等返回空表）。先按行定位改动，再在改动块内细化到字符。
pub fn diff_hunks(base: &str, next: &str) -> Vec<TextHunk> {
    if base == next {
        return Vec::new();
    }
    refine_hunks(base, hunks_from_tokens(&tokenize(base), &tokenize(next)))
}

/// 两条同锚点纯插入是否共享行：共享行时按并集拼接会把同一行写成两遍（静默重复内容），
/// 这种情形交回冲突更安全（调用方拒绝写入，本端内容仍在）。
fn inserts_share_lines(a: &str, b: &str) -> bool {
    let left: std::collections::HashSet<&str> = tokenize(a).into_iter().collect();
    tokenize(b).into_iter().any(|line| left.contains(line))
}

/// 同锚点两条纯插入该落哪一份：一侧的文本完整出现在另一侧（收敛后的正文已含对端已落盘
/// 那段，本端的新字可能落在它的前后或中间）时取更长一份——两侧文本都在，也不会写两遍；
/// 两条互不包含的插入返回 None，由调用方按序拼接。
fn dedupe_anchor_insert<'a>(a: &'a str, b: &'a str) -> Option<&'a str> {
    if a == b {
        return Some(a);
    }
    if a.len() > b.len() && a.contains(b) {
        return Some(a);
    }
    if b.len() > a.len() && b.contains(a) {
        return Some(b);
    }
    None
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
    // 同一锚点上的纯插入：一侧完整含另一侧时只落更长一份（收敛后的正文含对端内容）；
    // 独立插入按序拼接（内容都保留），共享行的独立插入判冲突（拼接会重复）
    if a.remove == 0 && b.remove == 0 {
        if a.at != b.at || dedupe_anchor_insert(&a.insert, &b.insert).is_some() {
            return false;
        }
        return inserts_share_lines(&a.insert, &b.insert);
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
            // 同锚点两条纯插入：一侧完整含另一侧（收敛后本端已含对端那段）时只落更长一份
            // ——拼接会把共有的文本写两遍
            if oh.at == th.at && oh.remove == 0 && th.remove == 0 {
                if let Some(text) = dedupe_anchor_insert(&oh.insert, &th.insert) {
                    out.push_str(&base[pos..oh.at]);
                    out.push_str(text);
                    pos = oh.at;
                    i += 1;
                    j += 1;
                    continue;
                }
            }
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
        // 先按行定位改动，再在改动块内按字符细化：只替换变了的字符，公共前后缀不进 hunk
        assert_eq!(
            diff_hunks("abc", "abXc"),
            vec![TextHunk { at: 2, remove: 0, insert: "X".into() }]
        );
        assert_eq!(
            diff_hunks("a\nb\nc\n", "a\nX\nc\n"),
            vec![TextHunk { at: 2, remove: 1, insert: "X".into() }]
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
        // 同一段文字被两侧改成不同内容：删除区间重叠，无法两侧都保留，按侧取舍
        let base = "aa\nbb\ncc\n";
        let ours = "aa\nOurs\ncc\n";
        let theirs = "aa\nTheirs\ncc\n";
        assert_eq!(merge3(base, &ours, &theirs, ConflictSide::Theirs), "aa\nTheirs\ncc\n");
        assert_eq!(merge3(base, &ours, &theirs, ConflictSide::Ours), "aa\nOurs\ncc\n");
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

    /// 两人在同一段落（同一行）的不同位置各改一处：行级差分把整行并成一个替换块，
    /// 两侧块区间重叠被误判冲突——协作笔记里同改一段就必然保存失败。块内按字符再差分后
    /// 两处改动各成一块，互不重叠即合流保留。
    #[test]
    fn 同一行两端改动合流而不判冲突() {
        let base = "共享段落\n";
        let ours = "共享段落甲\n";
        let theirs = "共享段落乙\n";
        assert!(!merge3_conflict(base, ours, theirs), "两处改动不相邻，不是冲突");
        assert_eq!(merge3(base, ours, theirs, ConflictSide::Ours), "共享段落甲乙\n");
        assert_eq!(
            plan_save(base, ours, theirs),
            SavePlan::Merge("共享段落甲乙\n".into()),
            "保存决策必须给合流而不是拒写"
        );

        // 单行正文（无换行）两端各追加：行级差分同样是整行替换
        assert_eq!(
            plan_save("记录", "记录甲", "记录乙"),
            SavePlan::Merge("记录甲乙".into())
        );

        // 长段落里两处相隔很远的改动：必须各自成块
        let long = "开头一句。中间这一句很长，用来把两处改动隔开。结尾一句。";
        let ours = long.replace("开头一句。", "开头一句（甲）。");
        let theirs = long.replace("结尾一句。", "结尾一句（乙）。");
        assert!(!merge3_conflict(long, &ours, &theirs));
        assert_eq!(
            merge3(long, &ours, &theirs, ConflictSide::Ours),
            "开头一句（甲）。中间这一句很长，用来把两处改动隔开。结尾一句（乙）。"
        );
    }

    /// 复现「多人同时编辑同一篇笔记 → 并发保存失败」的连续多轮：同一段落里两端各改一处，
    /// 每轮保存都必须给出合流结果且双方改动都在（不拒写、不重复写）。
    #[test]
    fn 多人同段落连续多轮保存都能合流() {
        let t0 = "会议纪要\n\n今天讨论了排期和预算两个议题。\n";
        // 甲改段首、乙改段尾：同一行两端，彼此都没收到对方改动
        let a = t0.replacen("今天讨论", "今天上午讨论", 1);
        let b = t0.replacen("两个议题。", "两个议题，下周一确认。", 1);
        // 甲先落盘；乙带旧基线到达，此时服务端已是甲的版本
        assert_eq!(plan_save(t0, &a, t0), SavePlan::Write(a.clone()));
        let merged = match plan_save(t0, &b, &a) {
            SavePlan::Merge(text) => text,
            other => panic!("同一段落的并发保存不得拒写：{other:?}"),
        };
        assert_eq!(
            merged,
            "会议纪要\n\n今天上午讨论了排期和预算两个议题，下周一确认。\n"
        );

        // 第二轮：都收到合流结果后继续在本段两处各自补写，落盘顺序重演
        let a2 = merged.replacen("今天上午讨论", "今天上午讨论（甲）", 1);
        let b2 = merged.replacen("下周一确认。", "下周一确认（乙）。", 1);
        assert_eq!(plan_save(&merged, &a2, &merged), SavePlan::Write(a2.clone()));
        let merged2 = match plan_save(&merged, &b2, &a2) {
            SavePlan::Merge(text) => text,
            other => panic!("第二轮并发保存不得拒写：{other:?}"),
        };
        assert!(merged2.contains("今天上午讨论（甲）"), "甲的改动不能丢：{merged2}");
        assert!(merged2.contains("下周一确认（乙）。"), "乙的改动不能丢：{merged2}");

        // 第三轮：段落末尾同一锚点各自追加，对端那句已含在本端内容里 → 只落一份
        let converged = format!("{merged2}新增丙。\n新增丁。\n");
        let peer = format!("{merged2}新增丙。\n");
        assert_eq!(
            plan_save(&merged2, &converged, &peer),
            SavePlan::Merge(converged.clone())
        );
    }

    /// 字符级细化只拆开互不相邻的改动，同一段文字被两侧改成不同内容仍是冲突：
    /// 拒绝写入让用户的输入留在编辑面，不得替它选边。
    #[test]
    fn 同字符区间两侧改写仍判冲突() {
        assert_eq!(plan_save("abc", "xbc", "ybc"), SavePlan::Conflict);
        assert_eq!(plan_save("共享段落\n", "共享甲\n", "共享乙\n"), SavePlan::Conflict);
    }

    #[test]
    fn 同锚点插入共享行判冲突() {
        // 两侧同一行后各插入一段，两段共享一行但不是前缀关系：按并集拼会写出两个 "X"，
        // 判冲突交调用方拒绝
        let base = "a\nb\n";
        let ours = "a\nX\nY\nb\n";
        let theirs = "a\nT\nX\nb\n";
        assert!(merge3_conflict(base, ours, theirs));
        // 不共享行仍按序拼接（两侧内容都保留）
        assert!(!merge3_conflict(base, "a\nP\nb\n", "a\nQ\nb\n"));
    }

    /// 协作收敛后本端内容已含对端已落盘的那段（一侧完整含另一侧）：同锚点只落更长一份，
    /// 不得拼接成重复文本，也不得判冲突拒写。
    #[test]
    fn 同锚点包含关系只落更长一份() {
        let base = "a\nb\n";
        let ours = "a\nX\nY\nb\n";
        let theirs = "a\nX\nb\n";
        assert!(!merge3_conflict(base, ours, theirs));
        assert_eq!(merge3(base, ours, theirs, ConflictSide::Ours), "a\nX\nY\nb\n");
        assert_eq!(
            plan_save(base, ours, theirs),
            SavePlan::Merge("a\nX\nY\nb\n".into())
        );

        // 段落末尾同一锚点各自追加：对端那句已含在本端内容里
        let base = "段落\n";
        let theirs = "段落甲\n";
        let ours = "段落甲乙\n";
        assert_eq!(plan_save(base, ours, theirs), SavePlan::Merge("段落甲乙\n".into()));
        // 反过来（本端落后于对端）同样取更长一份，两侧文本都在
        assert_eq!(plan_save(base, theirs, ours), SavePlan::Merge("段落甲乙\n".into()));

        // 对端在锚点处先插入「甲」并已落盘，本端随后在这段文字前后各写一句：
        // 本端插入完整含对端那段（不限于前缀），仍只落一份
        let base = "段落\n";
        let theirs = "段落甲\n";
        let ours = "段落乙甲丙\n";
        assert_eq!(plan_save(base, ours, theirs), SavePlan::Merge("段落乙甲丙\n".into()));
        let ours = "段落乙甲\n";
        assert_eq!(plan_save(base, ours, theirs), SavePlan::Merge("段落乙甲\n".into()));
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

    /// 超限中段里全是重复行（没有「两侧各出现一次」的行）时单行锚点为空，但这不代表两侧改动
    /// 重叠：按空行分隔的段落块切段后各改动各归一段。没有段落锚点就会退化成一整段替换，
    /// 把对侧落在其中的改动画进同区间，合并时只取一侧、对侧内容丢失。
    #[test]
    fn 长中段重复行按段落块切段保留两侧改动() {
        // 每段 12 行 a/b 位模式 + 空行：a、b、空行三种 token 全篇重复，整段拿不到唯一行锚点
        let mut lines: Vec<String> = Vec::new();
        for block in 0..400usize {
            for bit in 0..12 {
                lines.push(if (block >> bit) & 1 == 1 { "a\n".into() } else { "b\n".into() });
            }
            lines.push("\n".into());
        }
        let base = lines.concat();
        // 本端首末两处改动相距整篇：裁掉公共前后缀后中段仍超出 LCS 上限，进锚点切段分支；
        // 对端改在中段内部，退化成一整段替换时必然与本端 hunk 重叠
        let edit = |indices: [usize; 2], replacement: &str| -> String {
            let mut next = lines.clone();
            for index in indices {
                next[index] = replacement.to_owned();
            }
            next.concat()
        };
        let ours = edit([2 * 13, 390 * 13], "ours\n");
        // 对端两处改动相邻（合成一个 hunk），整体落在本端两处改动之间
        let theirs = edit([200 * 13, 200 * 13 + 1], "theirs\n");
        assert!(
            tokenize(&base).len() > MAX_DIFF_TOKENS,
            "夹具前提：改动落在长中段的差分路径上"
        );
        let our_hunks = diff_hunks(&base, &ours);
        assert_eq!(our_hunks.len(), 2, "两端各一处改动：{:?}", our_hunks);
        // 关键：每个 hunk 只覆盖被改的那一行内变化的字符，而不是把中间整段一起替换掉
        for hunk in &our_hunks {
            assert!(
                !base[hunk.at..hunk.at + hunk.remove].contains('\n'),
                "hunk 不得退化成整段替换：{:?}",
                hunk
            );
        }
        assert!(!diff_hunks(&base, &theirs).is_empty());
        assert!(!merge3_conflict(&base, &ours, &theirs), "三处改动互不重叠，不是冲突");
        let mut expected = lines.clone();
        expected[2 * 13] = "ours\n".into();
        expected[390 * 13] = "ours\n".into();
        expected[200 * 13] = "theirs\n".into();
        expected[200 * 13 + 1] = "theirs\n".into();
        assert_eq!(merge3(&base, &ours, &theirs, ConflictSide::Theirs), expected.concat());
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
