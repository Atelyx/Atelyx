/**
 * 协作空间内容后端：内容面契约 → 协作服务端 API（services/space/client 的 content 分组）。
 *
 * 当前覆盖：笔记域 + 画布/表格读写/补丁 + 附件/临时区/入库 + 结构变更（含引用同步）+ 索引。
 * 历史侧文件与仓库历史聚合不支持（`remapSideloads*` 为静默成功、`repoHistoryAggregate` 抛
 * `UnsupportedInSpaceError`），UI 据此禁用入口。
 *
 * 空间内媒体目录约定（服务端保留目录，树/索引不出现在结果中、读写可达）：
 * - 画布未入库临时附件：`.space-media/temp/<canvasId>/<fileName>`
 * - 表格图片：`.space-media/tables/<tableId>/<fileName>`
 * - 入库附件：`<附件文件夹>/<fileName>`（团队元数据 `attachment-folder` 设定，未配置 = `attachments/`；
 *   既有附件不因改动设定而迁移——引用是相对路径，改设定只影响之后入库的文件）
 *
 * 乐观锁冲突（写/补丁 409）镜像本地 Tauri 字符串错误形态（「画布/表格已被外部修改，请重载后再编辑」，
 * 与 Rust 命令文案逐字一致）：store 的冲突分支按错误文案判定（`includes("已被外部修改")`），
 * 形态不一致会让空间路径静默落进普通保存失败分支、丢失自动合并/冲突条行为。
 * 补丁端点 404（磁盘文件已被外部删除）同样镜像本地逐字文案——store 据此回退全量写；
 * 整文件写 404（路径级错误）镜像服务端消息（本地 safe_join 文案同形）。
 *
 * 引用改写（renameNote/renameFolder/表格与附件引用同步）前端复刻本地引擎的最小确定规则集：
 * 整词 `[[旧名]]` 替换、链接目标段 `(相对路径)` 替换、画布节点 `file` 字段精确替换；
 * 不确定的形态（含 `#` 锚点、大小写分歧、图片 `!` 语法）一律不改写。改写前用 grep 定位候选
 * 文件，逐个 read→替换→write，仅内容实际变化才写回（无关文件不被读写）。
 * 服务端补丁/重命名端点只改文件本身，画布引用同步由客户端在读改写中补齐（对应本地
 * rename/patch 命令里的事务化引用扫描，语义一致、时序上非事务）。
 *
 * 重建内部链接（rebuildLinks）同样复刻本地引擎规则：`[[名]]`/`[[名|别名]]` → `[名](规范路径)`、
 * 命中仓库 .md 的路径链接归一化、空路径按 label 补全；frontmatter/代码块/HTML 块/行内代码/
 * 图片链接/外部链接/非 .md 路径一律不动（见 rewriteInternalLinks 处注释）。
 */
import { READ_WINDOW_DEFAULT_LINES } from "@/constants/tools";
import { CANVAS_SCHEMA } from "@/constants/canvas";
import { SPACE_TEAM_META, spaceMetaScalar } from "@/constants/spaceMeta";
import { TABLE_SCHEMA } from "@/constants/table";
import { baseName, parentDir, sanitizeFilename, stripExt } from "@/utils/filename";
import { normalizeTableRow } from "@/utils/table";
import type {
  CanvasCreateResult,
  CanvasFile,
  CanvasFileRow,
  DeleteFolderResult,
  FileTreeNode,
  LinkRewriteResult,
  ListDirEntry,
  ListDirResult,
  ReadWindowResult,
  RebuildLinksResult,
  TableCreateResult,
  TableFile,
} from "@/types";
import {
  createSpaceClient,
  SpaceApiError,
  type MediaListEntry,
  type SpaceClient,
  type TreeNode,
} from "@/services/space/client";
import { getToken } from "@/services/space/auth";
import type { ContentBackend, TableImageSource } from "./contract";

/** 协作空间暂不支持的契约方法统一抛此错误，携带方法名供 UI 判定禁用入口。 */
export class UnsupportedInSpaceError extends Error {
  readonly feature: string;
  constructor(feature: string) {
    super("协作空间暂不支持该功能");
    this.name = "UnsupportedInSpaceError";
    this.feature = feature;
  }
}

function unsupported(feature: string): UnsupportedInSpaceError {
  return new UnsupportedInSpaceError(feature);
}

/** 409 冲突镜像的本地 Tauri 字符串错误文案（须与 Rust 命令逐字一致，见文件头注释）。 */
function conflictError(kind: "画布" | "表格"): string {
  return `${kind}已被外部修改，请重载后再编辑`;
}

/**
 * 服务端 404 → 本地同形字符串错误：本地 Tauri 命令错误均为字符串形态，store 的回退分支
 * 按 `typeof e === "string"` 判定；SpaceApiError（Error 实例）不命中会让空间路径静默落进
 * 普通保存失败分支、丢失回退全量写行为。补丁端点的文件缺失抛固定文案（与 Rust 命令逐字
 * 一致，防服务端措辞漂移破坏 store 判定）；整文件写本地从不报「文件不存在」（缺失 = 新建），
 * 其 404 只可能是路径级错误（父目录缺失/不可达），镜像服务端消息（与本地 safe_join 文案同形）。
 */
const PATCH_MISSING_ERRORS = {
  画布: "画布文件不存在（已从磁盘删除）",
  表格: "表格文件不存在（已从磁盘删除）",
} as const;

function patchMissingError(kind: keyof typeof PATCH_MISSING_ERRORS): string {
  return PATCH_MISSING_ERRORS[kind];
}

function pathLevelError(e: SpaceApiError): string {
  return e.serverMessage || e.message;
}

// ===== 空间媒体目录约定（见文件头注释） =====

const SPACE_MEDIA_DIR = ".space-media";
const SPACE_TEMP_DIR = `${SPACE_MEDIA_DIR}/temp`;
const SPACE_TABLE_MEDIA_DIR = `${SPACE_MEDIA_DIR}/tables`;
/** 入库附件的兜底目录（未配置「附件文件夹」时的落位）。 */
const SPACE_DEFAULT_ATTACHMENT_DIR = "attachments";

/**
 * 入库附件的目标目录：读团队元数据 `attachment-folder`（「附件文件夹」设定，与本地同语义）。
 *
 * 只接受仓库内普通相对目录——绝对路径、`..` 段、隐藏段（含服务端保留目录 `.space-media`）、
 * 以及 glob 元字符一律拒绝并抛错：隐藏目录不参与文件树与索引（附件落进去等于找不到），
 * 重名枚举按 glob 模式查已有附件、元字符会命中别的目录（同名附件会被当不存在而覆盖，附件丢失
 * 不可接受）。拒绝时报错而非回落默认目录：回落会让用户的设定静默失效，落错位置且无人知晓。
 */
function attachmentDirFromMeta(raw: string | undefined): string {
  const configured = spaceMetaScalar(raw).trim().replace(/^[/\\]+|[/\\]+$/g, "");
  if (!configured) return SPACE_DEFAULT_ATTACHMENT_DIR;
  const invalid =
    /^[a-zA-Z]:/.test(configured) ||
    configured.split(/[/\\]/).some((seg) => seg === "" || seg.startsWith(".")) ||
    /[*?[\]{}]/.test(configured);
  if (invalid) {
    throw new Error(
      `协作空间的附件文件夹设定无效：${configured}（请填写仓库内相对目录，不要用隐藏目录或通配符）`,
    );
  }
  return configured;
}

/** 按扩展名推 mime（仅图片，与本地 readAttachmentDataUrl 同口径；其余回落 application/octet-stream）。 */
function mimeFromExt(file: string): string {
  const ext = file.slice(file.lastIndexOf(".") + 1).toLowerCase();
  switch (ext) {
    case "png":
      return "image/png";
    case "jpg":
    case "jpeg":
      return "image/jpeg";
    case "webp":
      return "image/webp";
    case "gif":
      return "image/gif";
    default:
      return "application/octet-stream";
  }
}

/** 图片扩展名（importTableImage 落盘扩展名来源；非图片返回 null）。 */
function imageExtFromName(file: string): string | null {
  const ext = file.slice(file.lastIndexOf(".") + 1).toLowerCase();
  return ["png", "jpg", "jpeg", "webp", "gif"].includes(ext) ? ext : null;
}

/** 临时附件叶子名净化（与本地 write_temp_attachment 同口径：去路径段、替非法字符、空名兜底）。 */
function sanitizeTempFileName(fileName: string): string {
  const leaf = fileName.split(/[\\/]/).pop() ?? "";
  const cleaned = leaf
    .replace(/[/\\:*?"<>|\0]/g, "_")
    .trim()
    .replace(/^[.]+|[.]+$/g, "")
    .trim();
  return cleaned || "attachment";
}

/** 画布/表格落盘路径：目录（空 = 根）+ 净化标题 + 扩展名（与本地 create 命令同口径）。 */
function siblingEntityPath(dir: string, title: string, ext: "atlx" | "atb"): string {
  const name = `${sanitizeFilename(title)}.${ext}`;
  return dir ? `${dir}/${name}` : name;
}

/** 正则特殊字符转义（oldName/oldPath 可能含 `.`/`(` 等，避免误当元字符）。 */
function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 简易 percent 解码（%XX），非法序列原样保留（匹配不上自然不命中，不报错）。 */
function percentDecode(s: string): string {
  const bytes = new TextEncoder().encode(s);
  const out: number[] = [];
  let i = 0;
  while (i < bytes.length) {
    if (bytes[i] === 0x25 && i + 2 < bytes.length) {
      const hi = hexVal(bytes[i + 1]);
      const lo = hexVal(bytes[i + 2]);
      if (hi !== null && lo !== null) {
        out.push(hi * 16 + lo);
        i += 3;
        continue;
      }
    }
    out.push(bytes[i]);
    i += 1;
  }
  return new TextDecoder().decode(new Uint8Array(out));
}

function hexVal(b: number): number | null {
  if (b >= 0x30 && b <= 0x39) return b - 0x30;
  if (b >= 0x41 && b <= 0x46) return b - 0x41 + 10;
  if (b >= 0x61 && b <= 0x66) return b - 0x61 + 10;
  return null;
}

/**
 * 归一化链接目标：percent 解码、反斜杠→`/`、去 `./` 与前导 `/`；含 `..` 段返回 null（防越出仓库）。
 * 与本地引擎 `normalize_link_path` 同口径，保证改写判定一致。
 */
function normalizeLinkTarget(raw: string): string | null {
  let s = percentDecode(raw);
  s = s.replace(/\\/g, "/");
  while (s.startsWith("./")) s = s.slice(2);
  while (s.startsWith("/")) s = s.slice(1);
  if (!s) return null;
  for (const seg of s.split("/")) {
    if (seg === "..") return null;
  }
  return s;
}

/** 整词 wiki 链接替换：仅 `[[旧名]]` 与 `[[旧名|别名]]`，保留别名；`![[...]]` 嵌入语法不改写。 */
function rewriteWikiLinks(content: string, oldName: string, newName: string): string {
  const re = new RegExp(`(?<!!)\\[\\[(${escapeRegex(oldName)})((?:\\|[^\\]]*)?)\\]\\]`, "g");
  return content.replace(re, (_m, _name, rest) => `[[${newName}${rest}]]`);
}

/**
 * markdown 路径链接替换：仅 `[label](相对路径)` 且目标归一化后等于旧路径时改写；
 * `![...](...)` 图片链接与 `](#锚点)` 等不确定形态不改写。返回替换后的目标相对路径或 null。
 */
function rewritePathLinkTarget(
  target: string,
  oldPath: string,
  newPath: string,
): string | null {
  const norm = normalizeLinkTarget(target);
  if (norm === null || norm !== oldPath) return null;
  return target.replace(oldPath, newPath);
}

const PATH_LINK_RE = /(\!)?\[[^\]]*\]\(([^)]+)\)/g;

/** renameNote 引用改写：wiki 名替换 + 精确路径链接目标替换，两遍互不干扰。 */
function rewriteNoteReferences(
  content: string,
  ctx: { oldName: string; newName: string; oldPath: string; newPath: string },
): string {
  let out = rewriteWikiLinks(content, ctx.oldName, ctx.newName);
  out = out.replace(PATH_LINK_RE, (match, bang, target) => {
    if (bang) return match;
    const next = rewritePathLinkTarget(target, ctx.oldPath, ctx.newPath);
    if (next === null) return match;
    return replaceLinkTarget(match, next);
  });
  return out;
}

/** 把 `[label](target)` 中的 target 换成给定值（保留 label 与图片前缀）。 */
function replaceLinkTarget(link: string, newTarget: string): string {
  const m = link.match(/^(\!?)\[([^\]]*)\]\(([^)]*)\)$/);
  if (!m) return link;
  return `${m[1]}[${m[2]}](${newTarget})`;
}

/** renameFolder 引用改写：仅改写指向旧目录的路径链接（目标 = 旧目录或其子路径），wiki 名不变。 */
function rewriteFolderReferences(content: string, oldDir: string, newDir: string): string {
  return content.replace(PATH_LINK_RE, (match, bang, target) => {
    if (bang) return match;
    const norm = normalizeLinkTarget(target);
    if (norm === null) return match;
    if (norm === oldDir || norm.startsWith(`${oldDir}/`)) {
      return replaceLinkTarget(match, target.replace(oldDir, newDir));
    }
    return match;
  });
}

// ===== 重建内部链接（rebuildLinks）：复刻本地 rewrite_internal_links 的最小确定规则集 =====
//
// 规则（与本地引擎同口径，不确定的形态一律不动）：
// - `[[名]]` / `[[名|别名]]` → `[名](规范路径)` / `[别名](规范路径)`；目标笔记不存在 → `[名]()`（空路径快捷新建）；
// - `[label](路径)` 命中仓库内 .md（精确 → 文件名 → 同名取最短路径兜底 → 大小写不敏感兜底）→ 规范为精确路径；
// - `[名]()` 空路径按 label 解析，命中补全、未命中保持空路径；
// - 外部链接 / 含 `..` 的路径 / 非 .md 路径 / 图片链接（`!` 前缀）一律不动；
// - 只改写链接跨度：frontmatter、围栏代码块、缩进代码块、原始 HTML 块、行内代码整体跳过，
//   其余内容逐字符原样保留。

function atLineStart(content: string, i: number): boolean {
  return i === 0 || content[i - 1] === "\n";
}

/** 行首围栏开启长度（``` / ~~~，≥3 个同字符）。只认顶格：调用方传入的是未剥前导空格的行首，带缩进的围栏不识别（与本地引擎链接改写同口径）。 */
function fenceLenAt(content: string, i: number): number | null {
  const ch = content[i];
  if (ch !== "`" && ch !== "~") return null;
  let n = 0;
  while (content[i + n] === ch) n++;
  return n >= 3 ? n : null;
}

/** 在 rest 中找行首同字符闭合围栏（≥ openLen，前导空格 ≤3），返回闭合行末位置（含换行）。 */
function fenceCloseEnd(rest: string, openChar: string, openLen: number): number | null {
  // split("\n") 与逐行累加还原位置：除末段外每行含行尾换行（等价 Rust split_inclusive）
  const lines = rest.split("\n");
  let pos = 0;
  for (let idx = 0; idx < lines.length; idx++) {
    const start = pos;
    pos += lines[idx].length + (idx < lines.length - 1 ? 1 : 0);
    const trimmed = lines[idx].replace(/\r$/, "");
    const lead = trimmed.length - trimmed.replace(/^ +/, "").length;
    if (lead <= 3) {
      const body = trimmed.slice(lead);
      let n = 0;
      while (body[n] === openChar) n++;
      if (n >= openLen) return start + lines[idx].length + (idx < lines.length - 1 ? 1 : 0);
    }
  }
  return null;
}

/** 文档开头 frontmatter 结束位置（闭合 `---` 行之后）；无闭合返回 null。 */
function frontmatterEnd(content: string): number | null {
  const firstNl = content.indexOf("\n");
  if (firstNl < 0) return null;
  const lines = content.slice(firstNl + 1).split("\n");
  let pos = firstNl + 1;
  for (let idx = 0; idx < lines.length; idx++) {
    const lineEnd = pos + lines[idx].length + (idx < lines.length - 1 ? 1 : 0);
    pos = lineEnd;
    if (lines[idx].replace(/\r$/, "") === "---") return lineEnd;
  }
  return null;
}

/** 行内代码：反引号 run 长度。 */
function backtickRunAt(content: string, i: number): number {
  let n = 0;
  while (content[i + n] === "`") n++;
  return n;
}

/** 从 from 起找 ≥n 个连续反引号的闭合位置（run 末尾）；无闭合返回 null。 */
function backtickClose(content: string, from: number, n: number): number | null {
  let i = from;
  while (i < content.length) {
    if (content[i] === "`") {
      const run = backtickRunAt(content, i);
      if (run >= n) return i + run;
      i += run;
    } else {
      i += 1;
    }
  }
  return null;
}

/** 行首是否原始 HTML 块（标签块 / 注释 / 处理指令）。 */
function isHtmlBlockStart(line: string): boolean {
  return (
    line.startsWith("<!--") ||
    line.startsWith("<?") ||
    line.startsWith("<!") ||
    line.startsWith("</") ||
    (line.startsWith("<") && line.length > 1 && /[a-zA-Z]/.test(line[1]))
  );
}

/**
 * 链接跨度改写引擎：跳过 frontmatter/围栏代码/缩进代码/原始 HTML 块/行内代码/图片链接，
 * 对每个链接跨度调用 apply（返回原样 = 不改），其余内容逐字符原样保留。
 */
function rewriteLinkSpans(content: string, apply: (span: string) => string): string {
  let out = "";
  let i = 0;
  const len = content.length;
  if (content.startsWith("---\n") || content.startsWith("---\r\n")) {
    const end = frontmatterEnd(content);
    if (end !== null) {
      out += content.slice(0, end);
      i = end;
    }
  }
  while (i < len) {
    if (atLineStart(content, i)) {
      const rest = content.slice(i);
      const lead = rest.length - rest.replace(/^ +/, "").length;
      // 缩进代码块（≥4 空格）：跳过该行
      if (lead >= 4) {
        const nl = rest.indexOf("\n");
        const end = nl === -1 ? len : i + nl + 1;
        out += content.slice(i, end);
        i = end;
        continue;
      }
      const after = rest.slice(lead);
      if (isHtmlBlockStart(after)) {
        if (after.startsWith("<!--")) {
          // HTML 注释可跨行：跳至 `-->`（未闭合则余下整体跳过）
          const from = i + lead + 4;
          const p = content.indexOf("-->", from);
          const end = p === -1 ? len : p + 3;
          out += content.slice(i, end);
          i = end;
        } else {
          // 标签块：逐行跳过至空行（含空行）
          let cursor = i;
          for (;;) {
            const p = content.indexOf("\n", cursor);
            if (p === -1) {
              cursor = len;
              break;
            }
            if (content.slice(cursor, p).trim() === "") {
              cursor = p;
              break;
            }
            cursor = p + 1;
          }
          out += content.slice(i, cursor);
          i = cursor;
        }
        continue;
      }
      // 围栏代码块：跳至行首同字符闭合围栏（未闭合则不跳过；与本地引擎同口径只认行顶格开启）
      const openLen = fenceLenAt(content, i);
      if (openLen !== null) {
        const openChar = content[i];
        const close = fenceCloseEnd(content.slice(i + openLen), openChar, openLen);
        if (close !== null) {
          const end = i + openLen + close;
          out += content.slice(i, end);
          i = end;
          continue;
        }
      }
    }
    // 行内代码：反引号 run，跳至等长（或更长）run 闭合（未闭合则不跳过）
    if (content[i] === "`") {
      const n = backtickRunAt(content, i);
      const close = backtickClose(content, i + n, n);
      if (close !== null) {
        out += content.slice(i, close);
        i = close;
        continue;
      }
    }
    // wiki 链接 `[[..]]`：前导非 `!`（`![[a]]` 为嵌入语法，不按链接改写）
    if (content.startsWith("[[", i) && content[i - 1] !== "!") {
      const endRel = content.indexOf("]]", i + 2);
      if (endRel !== -1) {
        const end = endRel + 2;
        out += apply(content.slice(i, end));
        i = end;
        continue;
      }
    }
    // `[label](path)`：前导非 `!`（图片链接不处理）
    if (content[i] === "[" && content[i - 1] !== "!") {
      const closeLabel = content.indexOf("]", i + 1);
      if (closeLabel !== -1 && content[closeLabel + 1] === "(") {
        const closePath = content.indexOf(")", closeLabel + 2);
        if (closePath !== -1) {
          const end = closePath + 1;
          out += apply(content.slice(i, end));
          i = end;
          continue;
        }
      }
    }
    out += content[i];
    i += 1;
  }
  return out;
}

/** ASCII 大小写不敏感比较（Windows 文件系统语义，对齐 Rust eq_ignore_ascii_case）。 */
function asciiEqIgnoreCase(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  const fold = (s: string) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());
  return fold(a) === fold(b);
}

/**
 * 解析内部链接目标（笔记名或相对路径）→ 仓库内精确相对路径。
 * 与本地 resolve_link_target 同口径：候选 = 原样 .md 路径 / 补 .md / 净化名变体；
 * 匹配顺序 = 精确路径 → 文件名命中（同名取最短路径 + 字典序兜底）→ 大小写不敏感兜底。
 */
function resolveLinkTarget(
  name: string,
  exact: Set<string>,
  byBasename: Map<string, string[]>,
): string | null {
  const lowerEndsMd = name.replace(/[A-Z]/g, (c) => c.toLowerCase()).endsWith(".md");
  const candidates = lowerEndsMd ? [name] : [`${name}.md`];
  if (!lowerEndsMd) {
    const sanitized = sanitizeFilename(name);
    if (sanitized && sanitized !== name) candidates.push(`${sanitized}.md`);
  }
  const pick = (rels: string[]): string =>
    [...rels].sort((a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0))[0];
  for (const cand of candidates) {
    if (exact.has(cand)) return cand;
    const rels = byBasename.get(cand);
    if (rels?.length) return pick(rels);
  }
  for (const cand of candidates) {
    for (const hit of exact) {
      if (asciiEqIgnoreCase(hit, cand)) return hit;
    }
    for (const [key, rels] of byBasename) {
      if (asciiEqIgnoreCase(key, cand) && rels.length) return pick(rels);
    }
  }
  return null;
}

/** 取标准链接跨度 `[label](path)` 的 path 与 label（wiki 形式返回 null）。 */
function markdownLinkParts(span: string): { label: string; path: string } | null {
  if (span.startsWith("[[")) return null;
  const open = span.indexOf("](");
  if (open === -1) return null;
  return { label: span.slice(1, open), path: span.slice(open + 2, span.length - 1) };
}

/**
 * 一次性重建内部链接：返回 (新内容, 实际改写处数)。
 * `resolve`：笔记名或路径 → 命中时的仓库精确路径（null = 未命中）。
 */
function rewriteInternalLinks(
  content: string,
  resolve: (name: string) => string | null,
): [string, number] {
  let count = 0;
  const out = rewriteLinkSpans(content, (span) => {
    let replaced: string;
    if (span.startsWith("[[")) {
      const inner = span.slice(2, span.length - 2);
      const bar = inner.indexOf("|");
      const name = (bar === -1 ? inner : inner.slice(0, bar)).trim();
      const alias = (bar === -1 ? "" : inner.slice(bar + 1)).trim();
      const label = alias || name;
      const target = resolve(name);
      replaced = target === null ? `[${label}]()` : `[${label}](${target})`;
    } else {
      const parts = markdownLinkParts(span);
      if (parts === null) return span;
      const { label, path } = parts;
      if (path === "") {
        // `[名]()`：按 label 解析，命中补全；未命中保持空路径
        const target = resolve(label);
        if (target === null) return span;
        count += 1;
        return `[${label}](${target})`;
      }
      const norm = normalizeLinkTarget(path);
      if (norm === null) return span;
      const resolved = resolve(norm);
      // 未命中但仍是 .md 形状 → 空路径（快捷新建）；非 .md（附件等）不动
      if (resolved === null && !norm.replace(/[A-Z]/g, (c) => c.toLowerCase()).endsWith(".md")) {
        return span;
      }
      replaced = resolved === null ? `[${label}]()` : `[${label}](${resolved})`;
    }
    if (replaced !== span) count += 1;
    return replaced;
  });
  return [out, count];
}

// ===== 画布节点引用同步 =====

/**
 * 画布 JSON 内把 `file` 字段命中旧路径的值换成新路径（深度遍历；与本地 update_refs_in_canvas
 * 同语义——text/media/table 节点引用独立文件都用 `file`，按扩展名天然互斥）。
 * 返回是否有变更（就地修改）。
 */
function updateCanvasFileRefs(rootValue: unknown, oldFile: string, newFile: string): boolean {
  let changed = false;
  const visit = (v: unknown) => {
    if (Array.isArray(v)) {
      v.forEach(visit);
      return;
    }
    if (v && typeof v === "object") {
      const obj = v as Record<string, unknown>;
      if (obj.file === oldFile) {
        obj.file = newFile;
        changed = true;
      }
      Object.values(obj).forEach(visit);
    }
  };
  visit(rootValue);
  return changed;
}

/** 画布文本内引用临时区某目录的文件名集合；文本不是合法 JSON 时抛错（引用集合未知，不得当空集清理）。 */
function referencedTempNames(canvasText: string, dirPrefix: string): Set<string> {
  let value: unknown;
  try {
    value = JSON.parse(canvasText);
  } catch {
    throw new Error("画布文件损坏，无法解析，未回收临时附件");
  }
  const names = new Set<string>();
  const visit = (v: unknown) => {
    if (typeof v === "string") {
      if (v.startsWith(dirPrefix)) {
        const name = v.slice(dirPrefix.length);
        if (name && !name.includes("/")) names.add(name);
      }
      return;
    }
    if (Array.isArray(v)) {
      v.forEach(visit);
      return;
    }
    if (v && typeof v === "object") {
      Object.values(v).forEach(visit);
    }
  };
  visit(value);
  return names;
}


/** 在嵌套文件树中按相对路径定位节点（空串/根 = 顶层）。 */
function findTreeNode(nodes: TreeNode[], rel: string): TreeNode | null {
  if (!rel) return null;
  const parts = rel.split("/");
  let current: TreeNode | null = null;
  for (const part of parts) {
    current = (current === null ? nodes : current.children).find((n) => n.name === part) ?? null;
    if (!current) return null;
  }
  return current;
}

/** 服务端树节点 → FileTreeNode（形状直接映射，排除已由服务端完成）。 */
function mapTreeNode(n: TreeNode): FileTreeNode {
  return {
    name: n.name,
    path: n.path,
    isDir: n.isDir,
    updatedAt: n.updatedAt,
    children: n.children.map(mapTreeNode),
  };
}

/** 单层列举条目：目录在前、按名称升序；目录带直接子项数，文件无 size（服务端树不含字节数）。 */
function toListDirEntries(nodes: TreeNode[]): ListDirEntry[] {
  const sorted = [...nodes].sort(
    (a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name),
  );
  return sorted.map((n) =>
    n.isDir
      ? { name: n.name, kind: "dir", children: n.children.length }
      : { name: n.name, kind: "file" },
  );
}

/**
 * 创建协作空间内容后端。
 * @param serverUrl 协作服务器地址
 * @param spaceId 空间 id
 */
export function createSpaceContentBackend(serverUrl: string, spaceId: string): ContentBackend {
  const client: SpaceClient = createSpaceClient(serverUrl, () => getToken(serverUrl));

  /** 读文本，取后端 `{content, updatedAt}` 中的正文。 */
  async function readFileContent(file: string): Promise<string> {
    const res = await client.content.readFile(spaceId, file);
    return res.content;
  }

  /** 写文本（写失败如实抛出，由 SpaceApiError 携带状态码与消息）。 */
  async function writeFileContent(file: string, content: string): Promise<void> {
    await client.content.writeFile(spaceId, { path: file, content });
  }

  /** base64 读（附件/媒体二进制；文本读会损坏字节）。 */
  async function readFileBase64(file: string): Promise<string> {
    const res = await client.content.readFile(spaceId, file, { encoding: "base64" });
    return res.content;
  }

  /** base64 写（服务端解码后按字节落盘，限额按解码后字节计）。 */
  async function writeBase64File(file: string, base64Data: string): Promise<void> {
    await client.content.writeFile(spaceId, { path: file, content: base64Data, encoding: "base64" });
  }

  /** 文件是否存在（404 = 不存在；其余错误如实抛出）。 */
  async function fileExists(file: string): Promise<boolean> {
    try {
      await client.content.readFile(spaceId, file);
      return true;
    } catch (e) {
      if (e instanceof SpaceApiError && e.status === 404) return false;
      throw e;
    }
  }

  /** 读 JSON 实体（画布/表格）：文本 → JSON.parse，损坏如实抛错（错误含路径）。 */
  async function readEntityJson(
    file: string,
    kind: "画布" | "表格",
  ): Promise<{ data: Record<string, unknown>; updatedAt: number }> {
    const res = await client.content.readFile(spaceId, file);
    try {
      return { data: JSON.parse(res.content) as Record<string, unknown>, updatedAt: res.updatedAt };
    } catch {
      throw new Error(`${kind}文件损坏，无法解析：${file}`);
    }
  }

  /** 单层枚举保留媒体目录条目；目录不存在（404）视为空。仅用于 `.space-media/` 内路径——服务端 media/list 拒绝仓库可见目录（400）。 */
  async function listMediaEntries(path: string): Promise<MediaListEntry[]> {
    try {
      const res = await client.content.mediaList(spaceId, path);
      return res.entries;
    } catch (e) {
      if (e instanceof SpaceApiError && e.status === 404) return [];
      throw e;
    }
  }

  /** 用 grep 定位引用旧名/旧路径的候选文件（去重返回路径）。 */
  async function findReferenceFiles(patterns: string[], include: string): Promise<string[]> {
    const found = new Set<string>();
    for (const pattern of patterns) {
      const res = await client.content.grep(spaceId, { pattern, include });
      for (const m of res.matches) found.add(m.path);
    }
    return [...found];
  }

  /**
   * 画布引用同步：grep 定位含旧路径的 .atlx → read → 改写 `file` 字段命中值 → write。
   * 对应本地 rename/patch 命令的 collect_ref_updates（服务端 rename/补丁只改文件本身）。
   * 损坏画布跳过不阻塞其余（与本地链接维护同口径）。
   */
  async function rewriteCanvasRefs(oldFile: string, newFile: string): Promise<void> {
    const files = await findReferenceFiles([escapeRegex(oldFile)], "*.atlx");
    for (const file of files) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(await readFileContent(file));
      } catch {
        continue;
      }
      if (updateCanvasFileRefs(parsed, oldFile, newFile)) {
        await writeFileContent(file, JSON.stringify(parsed));
      }
    }
  }

  /** 全仓库画布文本集合（引用扫描/清理白名单来源）；任一画布解析失败即抛错（引用集合未知时不得当完整集）。 */
  async function readAllCanvasTexts(): Promise<Array<{ file: string; text: string }>> {
    const res = await client.content.glob(spaceId, { pattern: "**/*.atlx" });
    const out: Array<{ file: string; text: string }> = [];
    for (const file of res.paths) {
      let text: string;
      try {
        text = await readFileContent(file);
      } catch (e) {
        if (e instanceof SpaceApiError && e.status === 404) continue; // 列举后已被删除
        throw e;
      }
      JSON.parse(text); // 损坏画布如实抛错（引用集合未知，放弃清理）
      out.push({ file, text });
    }
    return out;
  }

  /**
   * 重写 .atlx/.atb 副本的 id 为全新值（防同 id 双文件歧义）；读/解析/写任一步失败即删除副本文件
   * （恢复复制前状态）再抛错——残留旧 id 的副本会被补丁守卫/协作合并当原件串写，不得静默保留。
   */
  async function regenerateEntityId(file: string): Promise<void> {
    let data: Record<string, unknown>;
    try {
      const res = await client.content.readFile(spaceId, file);
      data = JSON.parse(res.content) as Record<string, unknown>;
    } catch (e) {
      await discardFailedCopy(file, e);
      return;
    }
    if (typeof data.id !== "string") return;
    const oldId = data.id;
    while (data.id === oldId) data.id = crypto.randomUUID();
    try {
      await client.content.writeFile(spaceId, { path: file, content: JSON.stringify(data) });
    } catch (e) {
      await discardFailedCopy(file, e);
    }
  }

  /** 删除 id 重生成失败的副本后抛出明确错误；副本删除本身失败也如实记日志（残留可见，不静默）。 */
  async function discardFailedCopy(file: string, cause: unknown): Promise<void> {
    const reason = cause instanceof Error ? cause.message : String(cause);
    try {
      await client.content.deleteFile(spaceId, file);
    } catch (delErr) {
      console.error(`删除 id 重生成失败的副本文件失败：${file}`, delErr);
    }
    throw new Error(`复制文件 id 重生成失败，已删除副本：${file}（原因：${reason}）`);
  }

  /** 入库附件唯一落位路径：`<附件目录>/<基础名>`，重名追加 ` (n)`（与本地 unique_attachment_rel 同口径，不覆盖已有附件）。 */
  async function uniqueAttachmentRel(dir: string, fileName: string): Promise<string> {
    const dot = fileName.lastIndexOf(".");
    const hasExt = dot > 0 && dot < fileName.length - 1;
    const stem = hasExt ? fileName.slice(0, dot) : fileName;
    const ext = hasExt ? fileName.slice(dot + 1) : null;
    const withExt = (name: string) => (ext ? `${name}.${ext}` : name);
    // 重名枚举走内容面 glob：附件目录是仓库可见目录，media/list 对其拒绝（仅允许 .space-media/ 内）
    const res = await client.content.glob(spaceId, { pattern: `${dir}/*` });
    const existing = new Set(res.paths.map((p) => baseName(p)));
    let leaf = withExt(stem);
    let n = 1;
    while (existing.has(leaf)) {
      leaf = withExt(`${stem} (${n})`);
      n += 1;
    }
    return `${dir}/${leaf}`;
  }

  /**
   * 整体写画布/表格（带乐观锁基准）：内容序列化为 .atlx/.atb 全量 JSON 走 PUT /file。
   * 409 冲突镜像成本地同形字符串错误（store 冲突分支按文案判定），其余错误如实抛出。
   */
  async function writeEntityWithBase(
    file: string,
    entity: unknown,
    baseUpdatedAt: number | undefined,
    kind: "画布" | "表格",
  ): Promise<number> {
    try {
      const res = await client.content.writeFile(spaceId, {
        path: file,
        content: JSON.stringify(entity),
        ...(baseUpdatedAt !== undefined ? { baseUpdatedAt } : {}),
      });
      return res.updatedAt;
    } catch (e) {
      if (e instanceof SpaceApiError && e.status === 409) throw conflictError(kind);
      // 404 = 路径级错误（父目录缺失/不可达）：镜像本地字符串形态，store 统一按字符串错误处理
      if (e instanceof SpaceApiError && e.status === 404) throw pathLevelError(e);
      throw e;
    }
  }

  return {
    // ===== 树与列举 =====
    async listTree() {
      const nodes = await client.content.getTree(spaceId);
      return nodes.map(mapTreeNode);
    },
    async listDir(dir?: string) {
      const nodes = await client.content.getTree(spaceId);
      const node = dir && dir.length ? findTreeNode(nodes, dir) : null;
      const children = node ? node.children : nodes;
      return {
        entries: toListDirEntries(children),
        total: children.length,
        capped: false,
      } satisfies ListDirResult;
    },

    // ===== 读 =====
    readFile: readFileContent,
    async readFileWindow(file: string, opts?: { offset?: number; limit?: number }): Promise<ReadWindowResult> {
      const content = await readFileContent(file);
      const lines = content.split("\n");
      const offset = Math.max(1, opts?.offset ?? 1);
      const limit = opts?.limit ?? READ_WINDOW_DEFAULT_LINES;
      const window = lines.slice(offset - 1, offset - 1 + limit);
      return {
        lines: window.map((text, i) => ({ number: offset + i, text })),
        totalLines: lines.length,
        truncated: offset - 1 + limit < lines.length,
      };
    },
    readNote: readFileContent,

    // ===== 读（画布/表格/附件）=====
    async readCanvas(file: string): Promise<CanvasFile> {
      const { data, updatedAt } = await readEntityJson(file, "画布");
      // updatedAt 取 readFile 响应（服务端内容版本号）：store 把它当乐观锁基准传回 write/patch
      return { ...(data as unknown as CanvasFile), updatedAt };
    },
    async readTable(file: string): Promise<TableFile> {
      const { data, updatedAt } = await readEntityJson(file, "表格");
      const t = data as unknown as TableFile;
      if (!Array.isArray(t.rows)) throw new Error(`表格文件损坏，无法解析：${file}`);
      // 磁盘→内存唯一咽喉：图片单元格旧形态 string[] 归一化（与本地后端同口径）
      return { ...t, updatedAt, rows: t.rows.map(normalizeTableRow) };
    },
    async listCanvases(): Promise<CanvasFileRow[]> {
      // O(n) 全量读：画布数量级小（个位/十位），全量读换取零新服务端端点；损坏画布跳过不阻塞列表
      const res = await client.content.glob(spaceId, { pattern: "**/*.atlx" });
      const rows = await Promise.all(
        res.paths.map(async (file): Promise<CanvasFileRow | null> => {
          try {
            const { data } = await readEntityJson(file, "画布");
            return {
              id: typeof data.id === "string" ? data.id : "",
              title: typeof data.title === "string" ? data.title : "",
              file,
              updatedAt: typeof data.updatedAt === "number" ? data.updatedAt : 0,
            };
          } catch {
            return null;
          }
        }),
      );
      return rows
        .filter((r): r is CanvasFileRow => r !== null)
        .sort((a, b) => b.updatedAt - a.updatedAt);
    },
    async readAttachmentDataUrl(file: string): Promise<string> {
      const b64 = await readFileBase64(file);
      return `data:${mimeFromExt(file)};base64,${b64}`;
    },

    // ===== 写 =====
    writeFile: writeFileContent,
    writeNote: writeFileContent,

    // ===== 写（画布/表格整体写，乐观锁基准透传服务端）=====
    writeCanvas: (canvas, file, baseUpdatedAt) =>
      writeEntityWithBase(file, canvas, baseUpdatedAt, "画布"),
    writeTable: (table, file, baseUpdatedAt) =>
      writeEntityWithBase(file, table, baseUpdatedAt, "表格"),
    // 新建：最小磁盘 JSON 直接走 PUT /file（服务端写自动建父目录，无需先建夹）；
    // id 用 crypto.randomUUID（本地为 nanoid——同为不透明唯一串，协作按 id 相等性合并，形态无耦合）。
    async createCanvas(title: string, dir: string): Promise<CanvasCreateResult> {
      const id = crypto.randomUUID();
      const now = Math.floor(Date.now() / 1000);
      const canvas = {
        schema: CANVAS_SCHEMA,
        id,
        title,
        nodes: [],
        edges: [],
        createdAt: now,
        updatedAt: now,
      };
      const file = siblingEntityPath(dir, title, "atlx");
      // 与本地同策略：同名画布已存在即拒绝（正常路径由前端 dedupe 保证唯一，此处兜底防覆盖）
      if (await fileExists(file)) throw new Error(`画布名冲突：${title}`);
      await writeFileContent(file, JSON.stringify(canvas));
      return { id, file };
    },
    async createTable(title: string, dir: string): Promise<TableCreateResult> {
      const id = crypto.randomUUID();
      const now = Math.floor(Date.now() / 1000);
      const table = {
        schema: TABLE_SCHEMA,
        id,
        title,
        fields: [],
        rows: [],
        createdAt: now,
        updatedAt: now,
      };
      const file = siblingEntityPath(dir, title, "atb");
      if (await fileExists(file)) throw new Error(`表格名冲突：${title}`);
      await writeFileContent(file, JSON.stringify(table));
      return { id, file };
    },

    // ===== 增量补丁（409 冲突 / 404 文件缺失均镜像本地同形错误，成功返回写入后 {updatedAt, file}）=====
    patchCanvas: async (patch, file, baseUpdatedAt) => {
      try {
        const res = await client.content.patchCanvas(spaceId, {
          path: file,
          patch,
          ...(baseUpdatedAt !== undefined ? { baseUpdatedAt } : {}),
        });
        if (res.conflict) throw conflictError("画布");
        return { updatedAt: res.updatedAt, file: res.file };
      } catch (e) {
        // 文件被外部删除：store 按此字符串回退全量写（与本地 Tauri 命令文案逐字一致）
        if (e instanceof SpaceApiError && e.status === 404) throw patchMissingError("画布");
        throw e;
      }
    },
    patchTable: async (patch, file, baseUpdatedAt, force) => {
      try {
        const res = await client.content.patchTable(spaceId, {
          path: file,
          patch,
          ...(baseUpdatedAt !== undefined ? { baseUpdatedAt } : {}),
          ...(force ? { force: true } : {}),
        });
        if (res.conflict) throw conflictError("表格");
        // 表格改名漂移（title 变更 = 同目录改文件名）：服务端补丁只改文件本身，
        // 画布 table 节点引用同步由客户端改写补齐（对应本地 patch_table_vault 内的引用扫描）
        if (res.file !== file) await rewriteCanvasRefs(file, res.file);
        return { updatedAt: res.updatedAt, file: res.file };
      } catch (e) {
        if (e instanceof SpaceApiError && e.status === 404) throw patchMissingError("表格");
        throw e;
      }
    },

    // ===== 结构变更 =====
    async renameNote(oldFile: string, newFile: string): Promise<LinkRewriteResult> {
      await client.content.rename(spaceId, { oldPath: oldFile, newPath: newFile });
      const oldName = stripExt(baseName(oldFile));
      const newName = stripExt(baseName(newFile));
      const oldPath = oldFile;
      const newPath = newFile;
      const wikiPattern = `\\[\\[${escapeRegex(oldName)}(?:\\||\\]\\])`;
      const pathPattern = `\\]\\([^)]*(?:${escapeRegex(oldPath)}|${escapeRegex(`${oldName}.md`)})`;
      const files = await findReferenceFiles([wikiPattern, pathPattern], "*.md");
      const rewritten: string[] = [];
      for (const file of files) {
        const content = await readFileContent(file);
        const next = rewriteNoteReferences(content, { oldName, newName, oldPath, newPath });
        if (next !== content) {
          await writeFileContent(file, next);
          rewritten.push(file);
        }
      }
      return { rewritten };
    },
    // renameCanvas：空间内无 .atlx 标题字段可改，退化为纯路径重命名（标题按文件名推算）。
    async renameCanvas(file: string, newTitle: string): Promise<void> {
      const dir = file.includes("/") ? file.slice(0, file.lastIndexOf("/")) : "";
      const name = `${newTitle}.atlx`;
      const newFile = dir ? `${dir}/${name}` : name;
      await client.content.rename(spaceId, { oldPath: file, newPath: newFile });
    },
    moveCanvas: (oldFile, newFile) =>
      client.content.rename(spaceId, { oldPath: oldFile, newPath: newFile }),
    // renameTable：同目录改文件名 + 同步画布 table 节点引用（引用改写复用 collect_ref_updates 语义）。
    async renameTable(file: string, newTitle: string): Promise<void> {
      const newFile = siblingEntityPath(parentDir(file), newTitle, "atb");
      await client.content.rename(spaceId, { oldPath: file, newPath: newFile });
      await rewriteCanvasRefs(file, newFile);
    },
    async moveTable(oldFile: string, newFile: string): Promise<void> {
      await client.content.rename(spaceId, { oldPath: oldFile, newPath: newFile });
      await rewriteCanvasRefs(oldFile, newFile);
    },
    // renameAttachment：纯路径重命名 + 扫描 .atlx 更新 media 节点 file 引用（与本地 rename_attachment 对称）。
    async renameAttachment(oldFile: string, newFile: string): Promise<void> {
      await client.content.rename(spaceId, { oldPath: oldFile, newPath: newFile });
      await rewriteCanvasRefs(oldFile, newFile);
    },
    async renameFolder(oldDir: string, newDir: string): Promise<LinkRewriteResult> {
      await client.content.rename(spaceId, { oldPath: oldDir, newPath: newDir });
      const pathPattern = `\\]\\([^)]*${escapeRegex(oldDir)}`;
      const files = await findReferenceFiles([pathPattern], "*.md");
      const rewritten: string[] = [];
      for (const file of files) {
        const content = await readFileContent(file);
        const next = rewriteFolderReferences(content, oldDir, newDir);
        if (next !== content) {
          await writeFileContent(file, next);
          rewritten.push(file);
        }
      }
      return { rewritten };
    },
    deleteNote: (file) => client.content.deleteFile(spaceId, file),
    deleteAttachment: (file) => client.content.deleteFile(spaceId, file),
    // 删除画布/表格：不更新画布引用（契约如此，断链降级由前端处理）
    deleteCanvas: (file) => client.content.deleteFile(spaceId, file),
    deleteTable: (file) => client.content.deleteFile(spaceId, file),
    async deleteFolder(dir: string, force: boolean): Promise<DeleteFolderResult> {
      const res = await client.content.deleteFolder(spaceId, { path: dir, force });
      // 服务端仅返回 deleted / needsConfirm；空间无递归计数，itemCount 置 0（UI 仅作非空提示）
      return {
        deleted: res.deleted ?? false,
        needsConfirm: res.needsConfirm ?? false,
        itemCount: 0,
      };
    },
    async createFolder(dir: string): Promise<string> {
      const res = await client.content.createFolder(spaceId, { path: dir });
      return res.path;
    },

    // ===== 结构变更（复制 / 重建链接）=====
    // 复制文件：服务端纯字节复制后，.atlx/.atb 重新生成 id（本地语义：副本 id 必须全新，
    // 否则补丁防串文件守卫与协作按 id 合并会把副本当成原件串写）。
    async copyFile(oldFile: string, newFile: string): Promise<void> {
      await client.content.copy(spaceId, { fromPath: oldFile, toPath: newFile });
      if (oldFile.endsWith(".atlx") || oldFile.endsWith(".atb")) {
        await regenerateEntityId(newFile);
      }
    },
    // 复制目录：服务端 copy 递归复制整目录；复制后扫新目录内 .atlx/.atb 逐个重生成 id
    // （与本地 copy_folder 的 regenerate_ids_in 同语义）。
    async copyFolder(oldDir: string, newDir: string): Promise<void> {
      await client.content.copy(spaceId, { fromPath: oldDir, toPath: newDir });
      for (const ext of ["atlx", "atb"]) {
        const res = await client.content.glob(spaceId, { pattern: `**/*.${ext}` });
        for (const file of res.paths) {
          if (file.startsWith(`${newDir}/`)) await regenerateEntityId(file);
        }
      }
    },
    // 重建内部链接：glob 全仓库 .md → 逐个 read → 归一化内部链接 → 有变化才写。
    // 只读有链接跨度需要判定的文件；规则集与本地引擎的对应关系见 rewriteInternalLinks 注释。
    async rebuildLinks(): Promise<RebuildLinksResult> {
      const res = await client.content.glob(spaceId, { pattern: "**/*.md" });
      const exact = new Set(res.paths);
      const byBasename = new Map<string, string[]>();
      for (const rel of res.paths) {
        const base = baseName(rel);
        const rels = byBasename.get(base);
        if (rels) rels.push(rel);
        else byBasename.set(base, [rel]);
      }
      const resolve = (name: string) => resolveLinkTarget(name, exact, byBasename);
      let modified = 0;
      let links = 0;
      for (const file of res.paths) {
        let content: string;
        try {
          content = await readFileContent(file);
        } catch {
          continue; // 不可读跳过，不阻塞其余（与本地同口径）
        }
        const [next, n] = rewriteInternalLinks(content, resolve);
        if (n > 0) {
          links += n;
          try {
            await writeFileContent(file, next);
            modified += 1;
          } catch (e) {
            // 单文件写回失败跳过不阻塞其余（与本地「重建写回失败跳过该文件」同口径），
            // 但失败本身不可静默：留下可定位日志
            console.error(`重建内部链接写回失败（跳过该文件）：${file}`, e);
          }
        }
      }
      return { scanned: res.paths.length, modified, links };
    },

    // ===== 历史（协作空间无历史端点）=====
    remapSideloads: () => Promise.resolve(),
    remapSideloadsByDir: () => Promise.resolve(),

    // ===== 附件 =====
    // 未入库临时附件：唯一叶子名 `att-<随机>-<净化名>`（与本地同形），base64 写入后返回
    // 仓库相对路径引用（画布 media 节点 `file` 直接消费）。
    async writeTempAttachment(canvasId: string, fileName: string, base64Data: string): Promise<string> {
      const rel = `${SPACE_TEMP_DIR}/${canvasId}/att-${crypto.randomUUID()}-${sanitizeTempFileName(fileName)}`;
      await writeBase64File(rel, base64Data);
      return rel;
    },
    // 入库：复制临时件进「附件文件夹」设定的目录（未配置 = `attachments/`；重名追加 ` (n)`），
    // 画布改用返回路径引用。与本地 import_vault_attachment 同语义：复制而非移动——临时件可能被
    // 多个节点引用（节点复制粘贴），当场删源会断链；残留临时件由画布关闭时的按引用清理回收。
    async importAttachment(rel: string, fileName: string): Promise<{ file: string }> {
      const data = await readFileBase64(rel);
      const meta = await client.meta.getSpaceMeta(spaceId);
      const target = await uniqueAttachmentRel(
        attachmentDirFromMeta(meta.values?.[SPACE_TEAM_META.attachmentFolder]),
        sanitizeTempFileName(fileName),
      );
      await writeBase64File(target, data);
      return { file: target };
    },
    // 表格图片：唯一命名 `img-<随机>.<ext>`（与本地同形：删除后重导不覆盖旧文件、不撞缓存/撤销引用）。
    // 源为前端读好的本机文件 base64——本机路径在协作空间不可达，图片字节统一经前端读出后传输。
    async importTableImage(image: TableImageSource, tableId: string): Promise<string> {
      const ext = imageExtFromName(image.fileName);
      if (ext === null) throw new Error(`非图片文件：${image.fileName}`);
      const rel = `${SPACE_TABLE_MEDIA_DIR}/${tableId}/img-${crypto.randomUUID()}.${ext}`;
      await writeBase64File(rel, image.base64Data);
      return rel;
    },
    // 按引用回收画布临时附件：只删全仓库任何画布都不再引用的文件；
    // 引用扫描失败（画布读不到/损坏）一律放弃本次清理（残缺引用集当完整白名单会误删）。
    async cleanupCanvasTempAttachments(canvasId: string, canvasFile: string): Promise<number> {
      void canvasFile; // 目录归属由 canvasId 决定；画布自身引用已含在全仓库扫描中
      const dir = `${SPACE_TEMP_DIR}/${canvasId}`;
      const dirPrefix = `${dir}/`;
      const entries = await listMediaEntries(dir);
      if (entries.length === 0) return 0;
      let referenced: Set<string>;
      try {
        referenced = new Set();
        for (const { text } of await readAllCanvasTexts()) {
          for (const name of referencedTempNames(text, dirPrefix)) referenced.add(name);
        }
      } catch (e) {
        console.error("画布临时附件回收跳过（引用扫描失败）", e);
        return 0;
      }
      let removed = 0;
      for (const entry of entries) {
        if (referenced.has(entry.name)) continue;
        try {
          await client.content.deleteFile(spaceId, `${dir}/${entry.name}`);
          removed += 1;
        } catch (e) {
          console.error(`删除未引用临时附件失败：${dir}/${entry.name}`, e);
        }
      }
      return removed;
    },
    // 回收表格孤儿图片：删除附件目录中未被任一 image 单元格引用的文件；
    // 读盘失败（损坏/已被外部删除）返回 0 保守不清理——引用集合未知，防误删（与本地同口径）。
    async cleanupTableAttachments(file: string): Promise<number> {
      let table: Record<string, unknown>;
      try {
        ({ data: table } = await readEntityJson(file, "表格"));
      } catch {
        return 0;
      }
      const tableId = typeof table.id === "string" ? table.id : null;
      if (tableId === null || !Array.isArray(table.rows)) return 0;
      const imageFieldIds = new Set(
        (Array.isArray(table.fields) ? table.fields : [])
          .map((f) => f as { id?: unknown; type?: unknown })
          .filter((f) => f.type === "image" && typeof f.id === "string")
          .map((f) => f.id as string),
      );
      const referenced = new Set<string>();
      for (const row of table.rows as Array<{ values?: Record<string, unknown> }>) {
        for (const [fieldId, value] of Object.entries(row.values ?? {})) {
          if (!imageFieldIds.has(fieldId)) continue;
          const items = Array.isArray(value)
            ? value // 遗留 string[] 形态
            : value && typeof value === "object" && Array.isArray((value as { images?: unknown }).images)
              ? (value as { images: unknown[] }).images
              : [];
          for (const item of items) {
            if (typeof item === "string" && !item.startsWith("data:")) referenced.add(item);
          }
        }
      }
      const dir = `${SPACE_TABLE_MEDIA_DIR}/${tableId}`;
      const entries = await listMediaEntries(dir);
      if (entries.length === 0) return 0;
      let removed = 0;
      for (const entry of entries) {
        const rel = `${dir}/${entry.name}`;
        if (referenced.has(rel)) continue;
        try {
          await client.content.deleteFile(spaceId, rel);
          removed += 1;
        } catch (e) {
          console.error(`删除表格孤儿图片失败：${rel}`, e);
        }
      }
      return removed;
    },

    // ===== 索引 =====
    scanBacklinks: (noteName, noteFile) =>
      client.content.backlinks(spaceId, { noteName, noteFile }),
    scanTags: () => client.content.tags(spaceId),
    glob: (pattern, opts) => client.content.glob(spaceId, { pattern, path: opts?.path }),
    grep: (pattern, opts) => client.content.grep(spaceId, {
      pattern,
      path: opts?.path,
      include: opts?.include,
    }),

    // ===== 历史聚合（服务端无对应端点）=====
    repoHistoryAggregate: () => Promise.reject(unsupported("repoHistoryAggregate")),
  };
}
