//! 笔记正文三方合并（纯函数，不依赖 CRDT）：以最近落盘文本为共同祖先，
//! 把两侧差异都保留；冲突粒度为单个差异块（hunk 原子整体），重叠时按指定侧取舍。

/// 单个差异块：`base` 的 `[at, at + remove)` 字节区间替换为 `insert`（`remove = 0` 为纯插入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHunk {
    pub at: usize,
    pub remove: usize,
    pub insert: String,
}

/// 精确差分（LCS 表）的规模上限（单元格数）：更大的中段先按唯一公共行切分再逐段递归，
/// 避免长文档为省内存把整段降级成一个 hunk——那样对侧落在段内的不重叠改动会被整段吞掉。
const MAX_DIFF_CELLS: usize = 1_000_000;

/// 递归切分深度上限：超过即认定两侧无法安全对齐，交由调用方按冲突处理。
const MAX_SPLIT_DEPTH: u32 = 64;

/// 求差失败：两侧改动无法在规模上限内安全对齐（既无唯一公共行可切分，中段又超出精确差分规模）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unaligned;

/// 差异取向：重叠冲突时保留哪一侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
}

/// 按「含行尾换行的行」切 token：拼接 token 恒等于原文（末行可无换行）。
/// 每项带该行在全文中的字节起点，差异块据此换算源偏移。
fn tokenize(text: &str) -> Vec<(usize, &str)> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            tokens.push((start, &text[start..=i]));
            start = i + 1;
        }
    }
    if start < text.len() {
        tokens.push((start, &text[start..]));
    }
    tokens
}

/// 求 `a → b` 的差异块并追加到 `out`（`a` 为空时用 `empty_at` 作插入锚点）。
fn diff_tokens(
    a: &[(usize, &str)],
    b: &[(usize, &str)],
    empty_at: usize,
    depth: u32,
    out: &mut Vec<TextHunk>,
) -> Result<(), Unaligned> {
    let a_tokens: Vec<&str> = a.iter().map(|(_, token)| *token).collect();
    let b_tokens: Vec<&str> = b.iter().map(|(_, token)| *token).collect();
    let mut head = 0usize;
    while head < a.len() && head < b.len() && a_tokens[head] == b_tokens[head] {
        head += 1;
    }
    let mut tail = 0usize;
    while tail < a.len() - head
        && tail < b.len() - head
        && a_tokens[a.len() - 1 - tail] == b_tokens[b.len() - 1 - tail]
    {
        tail += 1;
    }
    let a_mid = &a[head..a.len() - tail];
    let b_mid = &b[head..b.len() - tail];
    let at = a.get(head).map(|(offset, _)| *offset).unwrap_or(empty_at);
    if a_mid.is_empty() && b_mid.is_empty() {
        return Ok(());
    }
    if a_mid.is_empty() {
        let insert: String = b_mid.iter().map(|(_, token)| *token).collect();
        out.push(TextHunk { at, remove: 0, insert });
        return Ok(());
    }
    if b_mid.is_empty() {
        let remove = a_mid.iter().map(|(_, token)| token.len()).sum();
        out.push(TextHunk { at, remove, insert: String::new() });
        return Ok(());
    }
    if a_mid.len().saturating_mul(b_mid.len()) <= MAX_DIFF_CELLS {
        lcs_hunks(a_mid, b_mid, at, out);
        return Ok(());
    }
    if depth >= MAX_SPLIT_DEPTH {
        return Err(Unaligned);
    }
    let anchors = unique_anchors(a_mid, b_mid);
    if anchors.is_empty() {
        return Err(Unaligned);
    }
    let mut pa = 0usize;
    let mut pb = 0usize;
    for (ai, bi) in anchors {
        diff_tokens(&a_mid[pa..ai], &b_mid[pb..bi], a_mid[ai].0, depth + 1, out)?;
        pa = ai + 1;
        pb = bi + 1;
    }
    // 末尾缺口：插入锚点是公共后缀的起点，没有公共后缀时才是切片末尾——
    // 取错锚点会把文末插入挪到后缀之后，hunk 应用到 base 就不再等于目标正文
    let tail_at = if tail > 0 { a[a.len() - tail].0 } else { empty_at };
    diff_tokens(&a_mid[pa..], &b_mid[pb..], tail_at, depth + 1, out)
}

/// 对中段做精确 LCS 差分（规模已由调用方限定），逐段合并为 hunk。
fn lcs_hunks(a_mid: &[(usize, &str)], b_mid: &[(usize, &str)], at: usize, out: &mut Vec<TextHunk>) {
    let a_tokens: Vec<&str> = a_mid.iter().map(|(_, token)| *token).collect();
    let b_tokens: Vec<&str> = b_mid.iter().map(|(_, token)| *token).collect();
    let n = a_tokens.len();
    let m = b_tokens.len();
    // table[i][j] = aMid[i..] 与 bMid[j..] 的 LCS 长度（单元格数 ≤ MAX_DIFF_CELLS，u32 足够）
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a_tokens[i] == b_tokens[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut hunk: Option<TextHunk> = None;
    let mut pos = at;
    let mut i = 0usize;
    let mut j = 0usize;
    while i < n || j < m {
        if i < n && j < m && a_tokens[i] == b_tokens[j] {
            if let Some(done) = hunk.take() {
                out.push(done);
            }
            pos += a_tokens[i].len();
            i += 1;
            j += 1;
            continue;
        }
        // 与 LCS 表一致的回溯方向：先插后删（相等时优先插入，保证 hunk 内含替换语义）
        if j < m && (i >= n || table[i][j + 1] >= table[i + 1][j]) {
            let h = hunk.get_or_insert_with(|| TextHunk { at: pos, remove: 0, insert: String::new() });
            h.insert.push_str(b_tokens[j]);
            j += 1;
            continue;
        }
        let h = hunk.get_or_insert_with(|| TextHunk { at: pos, remove: 0, insert: String::new() });
        h.remove += a_tokens[i].len();
        pos += a_tokens[i].len();
        i += 1;
    }
    if let Some(done) = hunk {
        out.push(done);
    }
}

/// 两侧都只出现一次的公共行：按 a 侧顺序取 b 侧递增的最长子序列，作为切分锚点。
fn unique_anchors(a: &[(usize, &str)], b: &[(usize, &str)]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;

    let mut count_a: HashMap<&str, (usize, usize)> = HashMap::new();
    for (index, (_, token)) in a.iter().enumerate() {
        let entry = count_a.entry(token).or_insert((0, index));
        entry.0 += 1;
    }
    let mut count_b: HashMap<&str, (usize, usize)> = HashMap::new();
    for (index, (_, token)) in b.iter().enumerate() {
        let entry = count_b.entry(token).or_insert((0, index));
        entry.0 += 1;
    }
    let pairs: Vec<(usize, usize)> = a
        .iter()
        .enumerate()
        .filter_map(|(index, (_, token))| {
            let (seen_in_a, _) = *count_a.get(token)?;
            let (seen_in_b, index_in_b) = *count_b.get(token)?;
            (seen_in_a == 1 && seen_in_b == 1).then_some((index, index_in_b))
        })
        .collect();
    longest_increasing(pairs)
}

/// 按第二项取最长严格递增子序列（输入已按第一项递增），返回原序的锚点对。
fn longest_increasing(pairs: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; pairs.len()];
    for index in 0..pairs.len() {
        let value = pairs[index].1;
        let at = tails.partition_point(|&slot| pairs[slot].1 < value);
        if at > 0 {
            previous[index] = Some(tails[at - 1]);
        }
        if at == tails.len() {
            tails.push(index);
        } else {
            tails[at] = index;
        }
    }
    let mut picked = Vec::with_capacity(tails.len());
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        picked.push(pairs[index]);
        cursor = previous[index];
    }
    picked.reverse();
    picked
}

/// 求 `base → next` 的差异块（相等返回空表）；无法安全对齐时只替换中段——
/// 公共前后缀仍留在原位（协作端据此保住 item 与光标锚点）。
/// 需要区分「能否安全合并」的调用方用 [`diff_hunks_checked`]。
pub fn diff_hunks(base: &str, next: &str) -> Vec<TextHunk> {
    match diff_hunks_checked(base, next) {
        Ok(hunks) => hunks,
        Err(Unaligned) => vec![middle_hunk(base, next)],
    }
}

/// 无法对齐时的整段替换块：裁掉逐行公共前后缀，只替换中段。
fn middle_hunk(base: &str, next: &str) -> TextHunk {
    let a = tokenize(base);
    let b = tokenize(next);
    let mut head = 0usize;
    while head < a.len() && head < b.len() && a[head].1 == b[head].1 {
        head += 1;
    }
    let mut tail = 0usize;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail].1 == b[b.len() - 1 - tail].1
    {
        tail += 1;
    }
    let at = a.get(head).map(|(offset, _)| *offset).unwrap_or(base.len());
    let remove = a[head..a.len() - tail].iter().map(|(_, token)| token.len()).sum();
    let insert = b[head..b.len() - tail].iter().map(|(_, token)| *token).collect();
    TextHunk { at, remove, insert }
}

/// 求 `base → next` 的差异块；两侧无法安全对齐时返回 [`Unaligned`]。
pub fn diff_hunks_checked(base: &str, next: &str) -> Result<Vec<TextHunk>, Unaligned> {
    if base == next {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    diff_tokens(&tokenize(base), &tokenize(next), base.len(), 0, &mut out)?;
    Ok(out)
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
/// 重叠冲突按 `side` 取一侧、另一侧与之重叠的 hunk 一并消费；两侧无法安全对齐时返回 [`Unaligned`]。
pub fn merge3_checked(
    base: &str,
    ours: &str,
    theirs: &str,
    side: ConflictSide,
) -> Result<String, Unaligned> {
    if ours == theirs {
        return Ok(ours.to_owned());
    }
    if base == ours {
        return Ok(theirs.to_owned());
    }
    if base == theirs {
        return Ok(ours.to_owned());
    }
    let our_hunks = diff_hunks_checked(base, ours)?;
    let their_hunks = diff_hunks_checked(base, theirs)?;

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
            (Some(oh), Some(th)) => oh.at < th.at || (oh.at == th.at && oh.remove == 0),
        };
        let next = if pick_ours {
            ours_hunk.expect("pick_ours 时 ours_hunk 必存在")
        } else {
            theirs_hunk.expect("否则 theirs_hunk 必存在")
        };
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
    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn joined(tokens: &[(usize, &str)]) -> String {
        tokens.iter().map(|(_, token)| *token).collect()
    }

    /// 把差异块按序应用到 base：必须逐字节等于目标正文。
    fn apply(base: &str, hunks: &[TextHunk]) -> String {
        let mut out = String::new();
        let mut pos = 0usize;
        for hunk in hunks {
            out.push_str(&base[pos..hunk.at]);
            out.push_str(&hunk.insert);
            pos = hunk.at + hunk.remove;
        }
        out.push_str(&base[pos..]);
        out
    }

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
        assert_eq!(joined(&tokenize("a\r\nb\nc")), "a\r\nb\nc");
        assert_eq!(joined(&tokenize("a\n\n")), "a\n\n");
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn 非重叠改动两侧保留() {
        let base = "line1\nline2\nline3\n";
        let ours = base.replace("line1", "ours1");
        let theirs = base.replace("line3", "theirs3");
        let merged = merge3_checked(base, &ours, &theirs, ConflictSide::Theirs).expect("可对齐");
        assert_eq!(merged, "ours1\nline2\ntheirs3\n");
        // 换侧调用对非重叠部分结果一致
        assert_eq!(merge3_checked(base, &ours, &theirs, ConflictSide::Ours).expect("可对齐"), merged);
    }

    #[test]
    fn 单侧未改动直接取另一侧() {
        assert_eq!(merge3_checked("base", "base", "theirs", ConflictSide::Ours).expect("可对齐"), "theirs");
        assert_eq!(merge3_checked("base", "ours", "base", ConflictSide::Theirs).expect("可对齐"), "ours");
        assert_eq!(merge3_checked("base", "same", "same", ConflictSide::Ours).expect("可对齐"), "same");
    }

    #[test]
    fn 同区间冲突按侧取舍() {
        let base = "aa\nbb\ncc\n";
        let ours = base.replace("bb", "ours-bb");
        let theirs = base.replace("bb", "theirs-bb");
        assert_eq!(merge3_checked(base, &ours, &theirs, ConflictSide::Theirs).expect("可对齐"), "aa\ntheirs-bb\ncc\n");
        assert_eq!(merge3_checked(base, &ours, &theirs, ConflictSide::Ours).expect("可对齐"), "aa\nours-bb\ncc\n");
    }

    #[test]
    fn 同起点插入与删除并存时两侧都保留() {
        // ours 纯插入在行首锚点，theirs 删除该行：锚点在删除区间端点上，不冲突
        let base = "keep\nvinyl\nkeep2\n";
        let ours = base.replace("keep\n", "keep\ninserted\n");
        let theirs = base.replace("vinyl\n", "");
        let merged = merge3_checked(base, &ours, &theirs, ConflictSide::Theirs).expect("可对齐");
        assert_eq!(merged, "keep\ninserted\nkeep2\n");
    }

    #[test]
    fn 完全相同改动幂等() {
        let base = "a\nb\n";
        let next = "a\nb\nc\n";
        let ours = next;
        let theirs = base.replace("b\n", "b\nc\n");
        assert_eq!(merge3_checked(base, ours, &theirs, ConflictSide::Theirs).expect("可对齐"), next);
    }

    #[test]
    fn 中文与多字节文本按字节偏移合并正确() {
        let base = "第一行\n第二行\n第三行\n";
        let ours = base.replace("第二行", "第二行（本端改）");
        let theirs = base.replace("第三行", "第三行（对端改）");
        let merged = merge3_checked(base, &ours, &theirs, ConflictSide::Theirs).expect("可对齐");
        assert_eq!(merged, "第一行\n第二行（本端改）\n第三行（对端改）\n");
    }

    /// 整段重写且与祖先没有公共行时无法三方对齐：不产出掺半正文，交回冲突。
    #[test]
    fn 整段重写无公共行时交回冲突() {
        let base = "x\n".repeat(2000);
        let ours: String = "y\n".repeat(2000);
        let theirs = format!("{base}tail\n");
        assert_eq!(diff_hunks_checked(&base, &ours), Err(Unaligned));
        assert_eq!(
            merge3_checked(&base, &ours, &theirs, ConflictSide::Theirs),
            Err(Unaligned),
            "无法对齐时只能交回冲突"
        );
    }

    /// 长文档中段超出精确差分规模时不得整段降级：对侧落在未被本端触及的行上的改动必须保留。
    #[test]
    fn 超限中段按唯一行切分仍保留对侧的不重叠改动() {
        let base: String = (0..2000).map(|i| format!("line{i}\n")).collect();
        // 本端隔行改动：中段规模远大于精确差分上限
        let mut ours = String::new();
        for i in 0..2000 {
            if i % 2 == 0 && i < 1300 {
                ours.push_str(&format!("ours{i}\n"));
            } else {
                ours.push_str(&format!("line{i}\n"));
            }
        }
        // 对端只改一行，且该行本端未动（1501 为奇数）
        let theirs = base.replace("line1501\n", "theirs1501\n");
        assert!(diff_hunks_checked(&base, &ours).is_ok());
        let merged = merge3_checked(&base, &ours, &theirs, ConflictSide::Ours).expect("唯一行可切分");
        assert!(merged.contains("theirs1501"), "对侧不重叠改动必须保留");
        assert!(merged.contains("ours0") && merged.contains("ours1298"));
        assert!(merged.contains("line1500"));
        // 差异块的基本不变量：应用到 base 必须还原目标正文
        for (base, next) in [(&base, &ours), (&ours, &theirs), (&base, &theirs)] {
            assert_eq!(apply(base, &diff_hunks(base, next)), *next, "hunk 应用应还原目标正文");
        }
    }

    /// 分段递归后末尾仍带公共后缀时，插入必须落在后缀之前（取错锚点会错序）。
    #[test]
    fn 末尾缺口插入锚在公共后缀之前() {
        let base: String = (0..2000).map(|i| format!("line{i}\n")).collect();
        // 中段隔行改动（规模超出精确差分上限）+ 把新行插在公共后缀之前
        let mut next = String::new();
        for i in 0..2000 {
            if i < 1300 && i % 2 == 0 {
                next.push_str(&format!("ours{i}\n"));
            } else {
                if i == 1300 {
                    next.push_str("EXTRA\n");
                }
                next.push_str(&format!("line{i}\n"));
            }
        }
        let hunks = diff_hunks_checked(&base, &next).expect("唯一行可切分");
        assert_eq!(apply(&base, &hunks), next, "末尾插入不得挪到公共后缀之后");
    }

    /// 无唯一公共行可切分时不得按单侧还原：必须交回冲突，由调用方保留两侧。
    #[test]
    fn 无法对齐时交回冲突而不是静默选边() {
        let base = "x\n".repeat(2000);
        let ours: String = "y\n".repeat(2000);
        let theirs = format!("{}z\n", "x\n".repeat(1999));
        assert_eq!(merge3_checked(&base, &ours, &theirs, ConflictSide::Ours), Err(Unaligned));
    }
}
