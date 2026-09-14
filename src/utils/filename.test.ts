/**
 * sanitizeFilename 与 Rust `sanitize_filename` 的对齐锁定（两侧同一张用例表：
 * `src-tauri/src/vault.rs` 的 `sanitize_filename_tests`，改一侧而另一侧未同步即失败）。
 * 保留名/尾点规则曾缺失，导致前端预测的落盘名与实际写盘名漂移（画布改名的保存目标指错文件）。
 */
import { describe, expect, it } from "vitest";
import { noteRenameTarget, sanitizeFilename } from "./filename";

describe("sanitizeFilename", () => {
  it("replaces illegal chars and trims", () => {
    expect(sanitizeFilename('a/b:c*d?"e<f>g|h')).toBe("a_b_c_d__e_f_g_h");
    expect(sanitizeFilename("  笔记  ")).toBe("笔记");
  });

  it("prefixes windows reserved names by first-dot stem", () => {
    expect(sanitizeFilename("con.md")).toBe("_con.md");
    expect(sanitizeFilename("CON")).toBe("_CON");
    expect(sanitizeFilename("lpt9.txt")).toBe("_lpt9.txt");
    expect(sanitizeFilename("icon.md")).toBe("icon.md");
  });

  it("suffixes trailing dot (trailing space is trimmed before the check, same as Rust)", () => {
    expect(sanitizeFilename("笔记.")).toBe("笔记._");
    expect(sanitizeFilename("笔记 ")).toBe("笔记");
    expect(sanitizeFilename("笔记")).toBe("笔记");
  });
});

describe("noteRenameTarget", () => {
  it("keeps the directory and appends the .md extension", () => {
    expect(noteRenameTarget("a/笔记.md", "新名")).toBe("a/新名.md");
    expect(noteRenameTarget("笔记.md", "新名")).toBe("新名.md");
  });

  it("sanitizes the title like the rename path does", () => {
    expect(noteRenameTarget("a/笔记.md", "a:b")).toBe("a/a_b.md");
    expect(noteRenameTarget("a/笔记.md", "con")).toBe("a/_con.md");
  });

  it("returns null when the target equals the current path", () => {
    expect(noteRenameTarget("a/笔记.md", "笔记")).toBeNull();
    expect(noteRenameTarget("a/笔记.md", " 笔记 ")).toBeNull();
    // 净化后与原路径相同（非法字符落在原文件名上）同样是无需改动
    expect(noteRenameTarget("a/a_b.md", "a:b")).toBeNull();
  });

  it("falls back to 未命名 when the title sanitizes to empty", () => {
    expect(noteRenameTarget("a/笔记.md", "  ")).toBe("a/未命名.md");
    expect(noteRenameTarget("a/笔记.md", "///")).toBe("a/___.md");
  });
});
