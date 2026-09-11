/**
 * 文件名工具。
 * 与 Rust 侧 `vault::sanitize_filename` 对齐（见 `src-tauri/src/vault.rs`），
 * 确保前端预生成的文件名与 Rust 落盘一致。
 */

/**
 * 净化文件名：替换 `/\:*?"<>|` 为 `_`，trim 首尾空白。
 * 与 Rust 侧 `sanitize_filename` 行为完全一致（见 `src-tauri/src/vault.rs`）：
 * Windows 保留名（CON/PRN/AUX/NUL/COM1-9/LPT1-9，按首个点前的 stem 判定）补 `_` 前缀、
 * 尾部点/空格补 `_` 后缀——缺这两条会使前端预测的落盘名与 Rust 实际写盘名不一致
 * （画布改名的保存目标/重命名验证会指错文件）。
 */
const WINDOWS_RESERVED_RE = /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/;

export function sanitizeFilename(title: string): string {
  const cleaned = title.replace(/[\/\\:*?"<>|]/g, "_").trim();
  const stem = cleaned.split(".")[0] ?? "";
  // 只折叠 ASCII 字母，与 Rust `to_ascii_uppercase` 对齐：用 `toUpperCase()` 会做 Unicode 大小写，
  // 且可能变长（`ß`→`SS`），使两侧保留名判定不一致
  const asciiUpper = stem.replace(/[a-z]/g, (c) => c.toUpperCase());
  if (WINDOWS_RESERVED_RE.test(asciiUpper)) return `_${cleaned}`;
  if (/[.]$/.test(cleaned)) return `${cleaned}_`;
  return cleaned;
}

/**
 * 防重名：若 name 与 existing 列表中某项冲突，追加 `-2`、`-3`... 直到不冲突。
 * 序号插在扩展名前：`笔记-2.md`（而非 `笔记.md-2`）；无扩展名则直接追加。
 * @example dedupeFilename("笔记.md", ["笔记.md"]) === "笔记-2.md"
 * @example dedupeFilename("笔记", ["笔记", "笔记-2"]) === "笔记-3"
 * @example dedupeFilename("a.b.md", ["a.b.md"]) === "a.b-2.md"
 * @example dedupeFilename("笔记.md", ["其他.md"]) === "笔记.md"
 */
export function dedupeFilename(name: string, existing: string[]): string {
  const set = new Set(existing);
  if (!set.has(name)) return name;
  // 拆 stem/ext（取最后扩展名分隔点），序号插在 ext 前
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  let i = 2;
  while (set.has(`${stem}-${i}${ext}`)) i++;
  return `${stem}-${i}${ext}`;
}

/** 路径末段文件名（"a/b.md" → "b.md"；根目录文件 → 自身）。 */
export function baseName(file: string): string {
  const i = file.lastIndexOf("/");
  return i >= 0 ? file.slice(i + 1) : file;
}

/** 去最后一个扩展名（"b.md" → "b"；无扩展名/隐藏文件原样返回）。 */
export function stripExt(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(0, dot) : name;
}

/** 笔记显示标题：路径末段文件名去 `.md` 后缀（"a/笔记.md" → "笔记"）。 */
export function noteTitleFromFile(file: string): string {
  return stripExt(baseName(file));
}

/** 表格显示标题：路径末段文件名去 `.atb` 后缀。 */
export function tableTitleFromFile(file: string): string {
  return stripExt(baseName(file));
}

/** 文件相对路径的父目录（"a/b.md" → "a"；根目录文件 → ""）。 */
export function parentDir(file: string): string {
  const i = file.lastIndexOf("/");
  return i > 0 ? file.slice(0, i) : "";
}

/** 同目录新文件名拼接（父目录 + 文件名）。 */
export function siblingPath(file: string, name: string): string {
  const dir = parentDir(file);
  return dir ? `${dir}/${name}` : name;
}

/**
 * 目录重命名后 remap 相对路径：路径位于 `oldDir/` 下 → `newDir/` + 剩余部分；否则原样返回。
 * 前缀含尾斜杠，防误匹配 `a` 命中 `ab/x.md`。
 */
export function remapDirPrefix(path: string, oldDir: string, newDir: string): string {
  const prefix = `${oldDir}/`;
  return path.startsWith(prefix) ? `${newDir}/${path.slice(prefix.length)}` : path;
}
