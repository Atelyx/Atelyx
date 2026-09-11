/**
 * sanitizeFilename 与 Rust `sanitize_filename` 的对齐锁定（两侧同一张用例表：
 * `src-tauri/src/vault.rs` 的 `sanitize_filename_tests`，改一侧而另一侧未同步即失败）。
 * 保留名/尾点规则曾缺失，导致前端预测的落盘名与实际写盘名漂移（画布改名的保存目标指错文件）。
 */
import { describe, expect, it } from "vitest";
import { sanitizeFilename } from "./filename";

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
