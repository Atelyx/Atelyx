/**
 * Frontmatter 对拍快照（TS ⇄ Rust 对拍的唯一期望源）。
 *
 * 双职能：
 * - 常规跑（pnpm test）= 守卫：gray-matter（js-yaml v3）解析/序列化结果须与
 *   `fixtures/frontmatter/parse.json` 快照逐值一致；
 * - `FM_DUMP=1 pnpm vitest run src/utils/frontmatter-fixture.test.ts` = 重新生成快照，
 *   Rust 侧（atelyx-core 对拍测试）以快照为期望。
 *
 * data 经 typed() 编码进快照：保留 js-yaml 的值类型（日期/嵌套/键序），
 * Rust 侧按同一编码重建比较（Date 用 UTC 日历日文本，见 formatDate）。
 */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { formatDate, parseFrontmatter, stringifyFrontmatter } from "./frontmatter";

/** 值类型化编码：t = 类型标签；map 用键值对数组保留 js-yaml 输出的键序。 */
function typed(v: unknown): unknown {
  if (v === null || v === undefined) return { t: "null" };
  if (v instanceof Date) return { t: "date", v: formatDate(v) };
  if (typeof v === "boolean") return { t: "bool", v };
  if (typeof v === "number") return { t: "num", v };
  if (typeof v === "string") return { t: "str", v };
  if (Array.isArray(v)) return { t: "seq", v: v.map(typed) };
  return { t: "map", v: Object.entries(v as Record<string, unknown>).map(([k, val]) => [k, typed(val)]) };
}

interface ParseCase {
  name: string;
  source: string;
}

interface StringifyCase {
  name: string;
  data: Record<string, unknown>;
  body: string;
}

const parseCorpus: ParseCase[] = [
  {
    name: "kitchen-sink",
    source: ["---", "title: Hello", "tags: [a, b]", "done: true", "count: 3", "due: 2024-01-15", "empty:", "---", "", "# 正文", ""].join("\n"),
  },
  { name: "tags-inline", source: "---\ntags: [a, b]\n---\nbody\n" },
  {
    name: "tags-block-list",
    source: ["---", "tags:", "  - alpha", "  - beta", "---", "body"].join("\n"),
  },
  { name: "tags-scalar", source: "---\ntags: foo\n---\n" },
  { name: "no-frontmatter", source: "# 正文\n内容 --- 分隔\n" },
  { name: "hr-only", source: "---\n\n正文横隔条\n" },
  { name: "unclosed", source: "---\ntitle: 未闭合\nbody\n" },
  { name: "empty-block", source: "---\n---\nbody\n" },
  { name: "empty-block-no-body", source: "---\n---\n" },
  { name: "crlf", source: "---\r\ntitle: 你好\r\n---\r\nbody\r\n" },
  { name: "crlf-empty", source: "---\r\n---\r\nx\r\n" },
  { name: "fm-then-hr-in-body", source: "---\ntitle: T\n---\n\n---\n\n正文\n" },
  { name: "colon-in-value", source: "---\ntitle: a: b\n---\n" },
  { name: "quoted-strings", source: "---\na: \"yes\"\nb: '123'\nc: \"null\"\nd: \"带 空格\"\n---\n" },
  { name: "numbers", source: "---\ni: 42\nf: 3.14\nneg: -7\nexp: 1e3\n---\n" },
  { name: "bools-and-null", source: "---\nyes: true\nno: false\nnothing: null\nnothing2:\n---\n" },
  { name: "dates", source: "---\ndue: 2024-01-15\ncreated: 2025-12-31\n---\n" },
  { name: "nested-map", source: "---\nmeta:\n  depth: 1\n  inner:\n    x: true\n---\n" },
  { name: "nested-seq", source: "---\nmatrix:\n  - [1, 2]\n  - [3, 4]\n---\n" },
  { name: "unicode-keys", source: "---\n中文键: 值\nmy key: v\n---\n" },
  { name: "key-order", source: "---\nzz: 1\naa: 2\nmm: 3\n---\n" },
  { name: "trailing-content-on-fence", source: "--- \ntitle: T\n--- x\nbody\n" },
  { name: "value-with-hash", source: "---\ntitle: #tag not\nother: \"#tag\"\n---\n" },
  { name: "body-starts-immediately", source: "---\ntitle: T\n---\n正文紧跟\n" },
  { name: "multiline-value", source: "---\ntext: \"line1\\nline2\"\n---\n" },
  { name: "long-value", source: "---\ntext: " + "长".repeat(600) + "\n---\nbody\n" },
  { name: "empty-seq-and-map", source: "---\nseq: []\nmap: {}\n---\n" },
];

const date = (y: number, m: number, d: number) => new Date(Date.UTC(y, m - 1, d));

const stringifyCorpus: StringifyCase[] = [
  {
    name: "kitchen-sink",
    data: { title: "Hello", tags: ["a", "b"], done: true, count: 3, due: date(2024, 1, 15) },
    body: "# 正文\n",
  },
  { name: "empty-data-no-block", data: {}, body: "# 正文\n" },
  { name: "body-no-trailing-newline", data: { title: "T" }, body: "正文无尾换行" },
  { name: "crlf-body", data: { title: "T" }, body: "第一行\r\n第二行\r\n" },
  { name: "empty-body", data: { title: "T" }, body: "" },
  { name: "empty-seq", data: { tags: [] }, body: "b\n" },
  { name: "null-value", data: { nothing: null }, body: "b\n" },
  { name: "floats", data: { f: 1.5, neg: -0.5, big: 1e21 }, body: "b\n" },
  { name: "bool-num", data: { yes: false, zero: 0 }, body: "b\n" },
  { name: "special-strings", data: { a: "yes", b: "123", c: "null", d: "a: b", e: "#tag", f: "- dash", g: "'quoted'", h: '"double"', i: " trailing ", j: " leading" }, body: "b\n" },
  { name: "unicode", data: { 中文: "值", key: "中文内容" }, body: "b\n" },
  { name: "multiline-string", data: { text: "line1\nline2" }, body: "b\n" },
  { name: "nested-map", data: { meta: { depth: 1, inner: { x: true } } }, body: "b\n" },
  { name: "nested-seq", data: { matrix: [[1, 2], [3, 4]] }, body: "b\n" },
  { name: "seq-of-maps", data: { items: [{ name: "a" }, { name: "b" }] }, body: "b\n" },
  { name: "date-and-seq-date", data: { due: date(2025, 12, 31), dates: [date(2024, 1, 1), date(2024, 6, 30)] }, body: "b\n" },
  { name: "special-keys", data: { "my key": 1, 中文键: 2, "a:b": 3 }, body: "b\n" },
  { name: "long-string", data: { text: "长".repeat(600) }, body: "b\n" },
  { name: "key-order-preserved", data: { zz: 1, aa: 2, mm: 3 }, body: "b\n" },
  { name: "empty-string-value", data: { a: "", b: " " }, body: "b\n" },
  { name: "mixed-tags-scalar", data: { tags: ["single"] }, body: "b\n" },
];

const FIXTURE_PATH = path.resolve(process.cwd(), "fixtures/frontmatter/parse.json");
const DUMP = !!process.env.FM_DUMP;

describe.skipIf(!DUMP)("frontmatter 对拍快照生成（FM_DUMP=1）", () => {
  it("全量语料写单文件快照", () => {
    const cases = [
      ...parseCorpus.map((c) => {
        const r = parseFrontmatter(c.source);
        return { name: c.name, source: c.source, parse: { data: typed(r.data), body: r.body, fmPrefix: r.fmPrefix, ok: r.ok } };
      }),
      ...stringifyCorpus.map((c) => ({
        name: `stringify:${c.name}`,
        stringify: { data: typed(c.data), body: c.body, out: stringifyFrontmatter(c.data, c.body) },
      })),
    ];
    writeFileSync(FIXTURE_PATH, JSON.stringify({ cases }, null, 2) + "\n");
  });
});

describe.skipIf(DUMP)("frontmatter 对拍快照守卫", () => {
  // 惰性读取：快照缺失的报错落在具体用例里，不阻塞 FM_DUMP=1 首次生成
  const load = (): { cases: unknown[] } => JSON.parse(readFileSync(FIXTURE_PATH, "utf8"));
  const byName = (name: string) => load().cases.find((c) => (c as { name: string }).name === name);

  it("语料规模兜底（防语料清单被误清空）", () => {
    expect(parseCorpus.length).toBeGreaterThan(10);
    expect(stringifyCorpus.length).toBeGreaterThan(10);
  });

  for (const c of parseCorpus) {
    it(c.name, () => {
      const fixture = byName(c.name) as { source: string; parse: { data: unknown; body: string; fmPrefix: string; ok: boolean } };
      const r = parseFrontmatter(fixture.source);
      expect(typed(r.data)).toEqual(fixture.parse.data);
      expect(r.body).toBe(fixture.parse.body);
      expect(r.fmPrefix).toBe(fixture.parse.fmPrefix);
      expect(r.ok).toBe(fixture.parse.ok);
    });
  }

  for (const c of stringifyCorpus) {
    it(`stringify:${c.name}`, () => {
      const fixture = byName(`stringify:${c.name}`) as { stringify: { out: string } };
      expect(stringifyFrontmatter(c.data, c.body)).toBe(fixture.stringify.out);
    });
  }
});
