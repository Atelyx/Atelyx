/**
 * 宽松换行关闭时的只读展示文本转换：同一段落内的单换行折叠为空格（标准 Markdown 语义）。
 *
 * 仅作用于只读展示面（MarkdownView：画布文本节点 / 对话气泡 / AI 面板）。
 * 笔记编辑器编辑与只读是同一视图、文本即真相，单换行恒为换行，不经此转换。
 *
 * 保守折叠：代码围栏、块级数学 `$$`、空白行、块级起始行（标题/引用/列表/分割线/围栏/脚注定义）、
 * 上一行硬换行（行尾 ≥2 空格）一律保留换行，避免破坏代码/公式/列表结构。
 */
export function collapseSoftLineBreaks(md: string): string {
  const lines = md.split("\n");
  let fence = "";
  let mathBlock = false;
  let out = "";
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? "";
    const trimmed = line.trim();
    // 围栏代码状态机（` ``` ` 或 ` ~~~ `，定界符 ≥3 个）
    const fm = /^(`{3,}|~{3,})/.exec(trimmed);
    if (fm) {
      if (!fence) fence = fm[1] ?? "";
      else if (line.startsWith(fence)) fence = "";
    }
    // 块级数学状态机（`$$` 行开/闭）
    if (/^\$\$/.test(trimmed)) mathBlock = !mathBlock;
    const inStructural = fence !== "" || mathBlock;
    if (i > 0) {
      const prev = lines[i - 1] ?? "";
      const prevTrim = prev.trim();
      const isBlank = trimmed === "";
      const isPrevBlank = prevTrim === "";
      const blockStart = (t: string) =>
        /^(#{1,6}[ \t]|>|[-+*][ \t]|\d+[.)][ \t]|`{3,}|~{3,}|\$\$|---+$|===+$|\[\^[^\]]+\]:)/.test(t);
      // 折叠条件：非结构性、两侧非空、两侧非块级起始、上一行无硬换行（行尾 ≥2 空格）
      const collapse =
        !inStructural &&
        !isBlank &&
        !isPrevBlank &&
        !blockStart(prevTrim) &&
        !blockStart(trimmed) &&
        !/ {2,}$/.test(prev);
      out += collapse ? " " : "\n";
    }
    out += line;
  }
  return out;
}
