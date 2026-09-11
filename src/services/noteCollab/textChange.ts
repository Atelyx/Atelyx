/**
 * 笔记正文文本差量与三方合并（纯函数，不依赖 Yjs）。
 *
 * 用途：
 * - `diffHunks`：把「当前正文 → 目标正文」表达为以字符区间为单位的 hunk 列表，
 *   供调用方在协作文档上做**增量**改写（整篇重写会把同一文本以不同客户端并存，合并即重复）。
 * - `merge3`：为「以最近落盘文本为共同祖先」的收敛提供三方合并，使本地未落盘输入与
 *   对端权威文本都能保留；仅当同一区间两侧都改写时按调用方指定的一侧取值（结果确定）。
 *
 * 精度：按「含行尾换行的行」切 token 后求 LCS，超限（`MAX_DIFF_TOKENS`）降级为
 * 「公共前后缀 + 中段整段替换」单 hunk——降级只影响粒度，不丢内容。
 */

/** 单个差异块：`base` 的 `[at, at + remove)` 区间替换为 `insert`（`remove = 0` 为纯插入）。 */
export interface TextHunk {
  at: number;
  remove: number;
  insert: string;
}

/** LCS 表规模上限（两侧中段各 ≤ 此值才走精确差分；超出走降级单 hunk，避免大文档卡顿）。 */
export const MAX_DIFF_TOKENS = 1200;

/** 按「含行尾换行的行」切 token：拼接 token 恒等于原文（末行可无换行）。 */
function tokenize(text: string): string[] {
  const tokens: string[] = [];
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) {
      tokens.push(text.slice(start, i + 1));
      start = i + 1;
    }
  }
  if (start < text.length) tokens.push(text.slice(start));
  return tokens;
}

/** 各 token 起始字符偏移（末位为全文长度），供 hunk 的字符区间定位。 */
function tokenOffsets(tokens: string[]): number[] {
  const offsets = new Array<number>(tokens.length + 1);
  offsets[0] = 0;
  for (let i = 0; i < tokens.length; i++) offsets[i + 1] = offsets[i] + tokens[i].length;
  return offsets;
}

/** 求 `a → b` 的差异块：裁掉公共前后缀后对中段做 LCS，逐段合并为 hunk。 */
function hunksFromTokens(a: string[], b: string[]): TextHunk[] {
  const aOffsets = tokenOffsets(a);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  ) {
    tail++;
  }
  const aMid = a.slice(head, a.length - tail);
  const bMid = b.slice(head, b.length - tail);
  const at = aOffsets[head];
  if (aMid.length === 0 && bMid.length === 0) return [];
  if (aMid.length === 0) return [{ at, remove: 0, insert: bMid.join("") }];
  if (bMid.length === 0) return [{ at, remove: aMid.join("").length, insert: "" }];
  if (aMid.length > MAX_DIFF_TOKENS || bMid.length > MAX_DIFF_TOKENS) {
    return [{ at, remove: aMid.join("").length, insert: bMid.join("") }];
  }

  const n = aMid.length;
  const m = bMid.length;
  // table[i][j] = aMid[i..] 与 bMid[j..] 的 LCS 长度（长度 ≤ MAX_DIFF_TOKENS，Uint16 足够）
  const table: Uint16Array[] = [];
  for (let i = 0; i <= n; i++) table.push(new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    const row = table[i];
    const below = table[i + 1];
    for (let j = m - 1; j >= 0; j--) {
      row[j] =
        aMid[i] === bMid[j] ? below[j + 1] + 1 : Math.max(below[j], row[j + 1]);
    }
  }

  const hunks: TextHunk[] = [];
  let hunk: TextHunk | null = null;
  let pos = at;
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && aMid[i] === bMid[j]) {
      if (hunk) {
        hunks.push(hunk);
        hunk = null;
      }
      pos += aMid[i].length;
      i++;
      j++;
      continue;
    }
    // 与 LCS 表一致的回溯方向：先插后删（相等时优先插入，保证 hunk 内含替换语义）
    if (j < m && (i >= n || table[i][j + 1] >= table[i + 1][j])) {
      hunk ??= { at: pos, remove: 0, insert: "" };
      hunk.insert += bMid[j];
      j++;
      continue;
    }
    hunk ??= { at: pos, remove: 0, insert: "" };
    hunk.remove += aMid[i].length;
    pos += aMid[i].length;
    i++;
  }
  if (hunk) hunks.push(hunk);
  return hunks;
}

/** 求 `base → next` 的差异块（`base === next` 返回空表）。 */
export function diffHunks(base: string, next: string): TextHunk[] {
  if (base === next) return [];
  return hunksFromTokens(tokenize(base), tokenize(next));
}

/** 两侧 hunk 是否落在同一区间：纯插入只在锚点**严格落在**对方区间内部时才算冲突。 */
function hunksOverlap(a: TextHunk, b: TextHunk): boolean {
  if (a.remove > 0 && b.remove > 0) {
    return a.at < b.at + b.remove && b.at < a.at + a.remove;
  }
  if (a.remove === 0 && b.remove === 0) return a.at === b.at;
  const insert = a.remove === 0 ? a : b;
  const remove = a.remove === 0 ? b : a;
  // 落在对方区间端点上的插入不冲突：先插后删（顺序分支的「同起点先插」）即两侧内容都保留
  return insert.at > remove.at && insert.at < remove.at + remove.remove;
}

/**
 * 三方合并：以 `base` 为共同祖先，把 `ours` / `theirs` 的差异都保留。
 *
 * 冲突粒度 = **hunk**：一个 hunk 的替换文本是原子整体（无法按子区间拆分），因此两侧 hunk 区间真正重叠时
 * 按 `conflict` 取一侧、另一侧与之重叠的 hunk 一并消费——结果只由入参决定（换侧调用得同一文本）。
 * 非重叠部分（含落在对方区间端点上的插入）两侧都保留。
 */
export function merge3(
  base: string,
  ours: string,
  theirs: string,
  conflict: "ours" | "theirs",
): string {
  if (ours === theirs) return ours;
  if (base === ours) return theirs;
  if (base === theirs) return ours;
  const ourHunks = diffHunks(base, ours);
  const theirHunks = diffHunks(base, theirs);

  let out = "";
  let pos = 0;
  let i = 0;
  let j = 0;
  // 循环不变式：hunk 表内区间有序且互不相交；跨侧同区间已在上一步按冲突规则消费完毕，
  // 故每次取用的 hunk 起点恒 ≥ pos（不会回退覆盖已写入区间）
  while (i < ourHunks.length || j < theirHunks.length) {
    const oursHunk = ourHunks[i];
    const theirsHunk = theirHunks[j];
    if (oursHunk && theirsHunk && hunksOverlap(oursHunk, theirsHunk)) {
      if (
        oursHunk.at === theirsHunk.at &&
        oursHunk.remove === theirsHunk.remove &&
        oursHunk.insert === theirsHunk.insert
      ) {
        out += base.slice(pos, oursHunk.at) + oursHunk.insert;
        pos = oursHunk.at + oursHunk.remove;
        i++;
        j++;
        continue;
      }
      // 同区间冲突只取指定一侧，另一侧与之重叠的 hunk 一并消费（结果仅由入参决定）
      if (conflict === "ours") {
        out += base.slice(pos, oursHunk.at) + oursHunk.insert;
        pos = oursHunk.at + oursHunk.remove;
        i++;
        while (theirHunks[j] && hunksOverlap(theirHunks[j]!, oursHunk)) j++;
      } else {
        out += base.slice(pos, theirsHunk.at) + theirsHunk.insert;
        pos = theirsHunk.at + theirsHunk.remove;
        j++;
        while (ourHunks[i] && hunksOverlap(ourHunks[i]!, theirsHunk)) i++;
      }
      continue;
    }
    // 同起点时先应用纯插入（插入锚点在对方区间端点，先插后删才不丢插入内容）
    const pickOurs =
      !theirsHunk ||
      (oursHunk !== undefined &&
        (oursHunk.at < theirsHunk.at ||
          (oursHunk.at === theirsHunk.at && oursHunk.remove === 0)));
    const next = pickOurs ? oursHunk! : theirsHunk;
    out += base.slice(pos, next.at) + next.insert;
    pos = next.at + next.remove;
    if (pickOurs) i++;
    else j++;
  }
  return out + base.slice(pos);
}
