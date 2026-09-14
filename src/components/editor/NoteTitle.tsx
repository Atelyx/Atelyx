/**
 * 页面内标题：笔记面板正文顶部的文件名标题（去扩展名），点击进入行内编辑，提交即重命名笔记文件。
 *
 * 标题恒由 `file` 派生（磁盘事实）：重命名成功后打开的路径随之变化，面板按路径重挂载，
 * 本组件不缓存标题。提交后到面板切到新路径之间显示「在途提交的目标标题」，防旧名回闪。
 *
 * 同名冲突在此前置拒绝：`vaultStore.renameNote` 对同名会静默加序号，而这里是用户手输的文档名，
 * 改完与他刚输入的内容不符，故不进入该逻辑，内联提示后由用户自行改名或另开标题。
 */
import { useRef, useState } from "react";
import { useVaultStore } from "@/stores/vaultStore";
import { noteRenameTarget, noteTitleFromFile } from "@/utils/filename";

export function NoteTitle({ file }: { file: string }) {
  const title = noteTitleFromFile(file);
  /** 编辑草稿（null = 显示态）。 */
  const [draft, setDraft] = useState<string | null>(null);
  /** 在途提交的目标标题（重命名落盘前顶替显示，失败即清）。 */
  const [committed, setCommitted] = useState<string | null>(null);
  /** 重命名失败/同名拒绝的内联提示（重新编辑时清除）。 */
  const [notice, setNotice] = useState<string | null>(null);
  /** 本次编辑已结束：Enter 提交后紧随的 blur 不得再触发一次重命名。 */
  const doneRef = useRef(false);

  const shown = committed ?? title;

  const beginEdit = () => {
    // 重命名在途：面板即将切到新路径，此间的编辑会落到已不存在的旧路径上
    if (committed) return;
    doneRef.current = false;
    setNotice(null);
    setDraft(shown);
  };

  /** 结束编辑：commit = 提交重命名（空标题/未变恒不提交），否则放弃（Esc）。 */
  const finishEdit = (commit: boolean) => {
    if (doneRef.current) return;
    doneRef.current = true;
    const text = draft ?? "";
    setDraft(null);
    if (!commit) return;
    const next = text.trim();
    if (!next || next === shown) return;
    const target = noteRenameTarget(file, next);
    // 无目标 = 净化后与原路径相同；目标已被同仓库笔记占用 = 拒绝（不覆盖、不自动加序号）
    if (!target) return;
    if (useVaultStore.getState().noteList.some((n) => n.file === target)) {
      setNotice(`「${noteTitleFromFile(target)}」已存在，未重命名`);
      return;
    }
    // 在途显示净化后的目标名：落盘用的就是它（用户输入含非法字符时两者并不相同）
    const targetTitle = noteTitleFromFile(target);
    setCommitted(targetTitle);
    void useVaultStore
      .getState()
      .renameNote(file, next)
      .then((actual) => {
        // 前置判定用的是内存树：树落后磁盘时链路会按磁盘防重名改成「名-2」，如实告知实际落点
        const actualTitle = noteTitleFromFile(actual);
        if (actualTitle !== targetTitle) setNotice(`「${targetTitle}」已存在，已重命名为「${actualTitle}」`);
      })
      .catch((e) => {
        console.error("重命名笔记失败", e);
        setCommitted(null);
        setNotice("重命名失败，请重试");
      });
  };

  return (
    <div className="flex-shrink-0 px-4 pt-3 pb-1">
      {draft === null ? (
        <button
          className="w-full text-left text-2xl font-semibold truncate rounded cursor-text hover:opacity-80"
          style={{ color: "var(--text-primary)" }}
          onClick={beginEdit}
          title="点击重命名笔记"
        >
          {shown}
        </button>
      ) : (
        <input
          autoFocus
          spellCheck={false}
          value={draft}
          placeholder={shown}
          onChange={(e) => setDraft(e.target.value)}
          onFocus={(e) => e.target.select()}
          onBlur={() => finishEdit(true)}
          onKeyDown={(e) => {
            // 输入法组合中的 Enter/Esc 属于候选上屏与取消组合，不作为提交/放弃
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter") finishEdit(true);
            else if (e.key === "Escape") finishEdit(false);
          }}
          className="w-full bg-transparent outline-none text-2xl font-semibold"
          style={{ color: "var(--text-primary)", borderBottom: "1px dashed var(--border)" }}
        />
      )}
      {notice && (
        <div className="text-xs mt-1" style={{ color: "#f87171" }}>
          {notice}
        </div>
      )}
    </div>
  );
}
