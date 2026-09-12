/**
 * 从 ctx 契约声明生成插件开发者 API 参考（docs/plugins/ctx-api.md 的服务表与事件表）。
 *
 * 契约唯一来源 = `src/services/cordis/types.ts`（声明合并的 Context / Events）+
 * `src/services/cordis/slotsApi.ts`（SlotsApi 接口不在 types.ts）；生成物以标记包裹，
 * 文档里的手写叙事不受影响。服务面清单一致性（ctx 声明 ↔ PLUGIN_SERVICE_LABELS）同批校验。
 * 用法：`node scripts/gen-ctx-api.mjs` 写回；`--check` 只校验，漂移即非零退出。
 */
import ts from "typescript";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DOC = "docs/plugins/ctx-api.md";
const TYPES = "src/services/cordis/types.ts";
const SLOTS_API = "src/services/cordis/slotsApi.ts";
const SERVICE_LIST = "src/constants/pluginServices.ts";
const MODULE_NAME = "@atelyx/cordis";
const BLOCKS = ["services", "events"];

const marker = (name, edge) => `<!-- generated:ctx-api:${name}:${edge} -->`;

async function readText(rel) {
  return readFile(resolve(ROOT, rel), "utf8");
}

function parse(text, rel) {
  return ts.createSourceFile(rel, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}

/** 压成单行（表格单元里不能有换行；多行对象类型字面量会被摊平）。 */
const flat = (text) => text.replace(/\s+/g, " ").trim();

/** 节点上的 JSDoc 块（取最后一个 = 紧邻声明的那块）。 */
function jsdocsOf(node) {
  return ts.getJSDocCommentsAndTags(node).filter((n) => ts.isJSDoc(n));
}

/** JSDoc 正文，压成单行（表格单元里不能有换行）。 */
function docText(node) {
  const docs = jsdocsOf(node);
  const raw = docs.length ? ts.getTextOfJSDocComment(docs[docs.length - 1].comment) : undefined;
  return (raw ?? "").replace(/\s+/g, " ").trim();
}

function docTagNames(node) {
  const docs = jsdocsOf(node);
  const tags = docs.length ? (docs[docs.length - 1].tags ?? []) : [];
  return tags.map((t) => t.tagName.text);
}

/** 正文里内联的分派标注（如「……。@emit」）不算 JSDoc 标签，需单独拆出。 */
function splitInlineTags(text) {
  const tags = [];
  const clean = text
    .replace(/@([A-Za-z][\w-]*)/g, (_match, tag) => {
      tags.push(tag);
      return "";
    })
    .replace(/\s+/g, " ")
    .trim();
  return { text: clean, tags };
}

/** 递归收集接口声明（Context/Events 在 declare module 内，SlotsApi 在顶层）。 */
function collectInterfaces(sf) {
  const map = new Map();
  const visit = (node) => {
    if (ts.isInterfaceDeclaration(node)) {
      map.set(node.name.text, {
        doc: docText(node),
        methods: node.members.filter((m) => ts.isMethodSignature(m)).map((m) => ({
          name: m.name.getText(sf),
          params: flat(m.parameters.map((p) => p.getText(sf)).join(", ")),
          ret: flat(m.type ? m.type.getText(sf) : "void"),
        })),
      });
    }
    node.forEachChild(visit);
  };
  sf.forEachChild(visit);
  return map;
}

function findModuleBlock(sf, name) {
  let found = null;
  const visit = (node) => {
    if (
      ts.isModuleDeclaration(node) &&
      ts.isStringLiteral(node.name) &&
      node.name.text === name &&
      node.body &&
      ts.isModuleBlock(node.body)
    ) {
      found = node.body;
      return;
    }
    node.forEachChild(visit);
  };
  sf.forEachChild(visit);
  if (!found) throw new Error(`未找到 declare module "${name}" 声明`);
  return found;
}

function findInterface(block, name) {
  const found = block.statements.find((s) => ts.isInterfaceDeclaration(s) && s.name.text === name);
  if (!found) throw new Error(`未找到接口声明：${name}`);
  return found;
}

/** PLUGIN_SERVICE_LABELS 的键（服务面唯一清单，用于与 ctx 声明交叉校验）。 */
function collectLabelKeys(sf) {
  let keys = null;
  const visit = (node) => {
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.name.text === "PLUGIN_SERVICE_LABELS" &&
      node.initializer &&
      ts.isObjectLiteralExpression(node.initializer)
    ) {
      keys = node.initializer.properties.map((p) => p.name.getText(sf));
      return;
    }
    node.forEachChild(visit);
  };
  sf.forEachChild(visit);
  if (!keys) throw new Error("未找到 PLUGIN_SERVICE_LABELS");
  return keys;
}

function renderTable(columns, rows) {
  const head = `| ${columns.join(" | ")} |`;
  const sep = `| ${columns.map(() => "---").join(" | ")} |`;
  return [head, sep, ...rows].join("\n");
}

function serviceRows(context, ifaces, sf) {
  return context.members
    .filter((m) => ts.isPropertySignature(m) && m.name)
    .map((m) => {
      const name = m.name.getText(sf);
      const iface = ifaces.get(m.type ? m.type.getText(sf) : "");
      const methods = iface?.methods.length
        ? iface.methods.map((x) => `\`${x.name}(${x.params}): ${x.ret}\``).join(" / ")
        : "—";
      return `| \`ctx.${name}\` | ${methods} | ${iface?.doc || "—"} |`;
    });
}

function eventRows(events, sf) {
  return events.members
    .filter((m) => ts.isPropertySignature(m) && m.name)
    .map((m) => {
      const name = ts.isStringLiteral(m.name) ? m.name.text : m.name.getText(sf);
      const fn = m.type && ts.isFunctionTypeNode(m.type) ? m.type : null;
      const payload =
        fn && fn.parameters.length > 0 && fn.parameters[0].type
          ? `\`${flat(fn.parameters[0].type.getText(sf))}\``
          : "—";
      const body = splitInlineTags(docText(m));
      const dispatch = [...docTagNames(m), ...body.tags][0] ?? "—";
      return `| \`${name}\` | ${payload} | ${dispatch} | ${body.text || "—"} |`;
    });
}

function blockContent(name, table) {
  return [
    marker(name, "begin"),
    "<!-- 本表由 ctx 类型契约自动生成，勿手改；改契约后运行 pnpm run ctx-api 重新生成。 -->",
    "",
    table,
    marker(name, "end"),
  ].join("\n");
}

function assertServiceList(contextNames, labelKeys) {
  const declared = [...contextNames].sort().join(",");
  const labels = [...labelKeys].sort().join(",");
  if (declared !== labels) {
    throw new Error(`服务面清单漂移：ctx 声明 [${declared}] ≠ 标签清单 [${labels}]`);
  }
}

/** 取标记之间的整块（含标记本身），供比对与替换。 */
function extractBlock(text, name) {
  const begin = marker(name, "begin");
  const end = marker(name, "end");
  const start = text.indexOf(begin);
  const stop = text.indexOf(end);
  if (start < 0 || stop < 0) throw new Error(`文档缺少生成块标记：${name}`);
  return text.slice(start, stop + end.length);
}

function replaceBlock(text, name, content) {
  const begin = marker(name, "begin");
  const end = marker(name, "end");
  const start = text.indexOf(begin);
  const stop = text.indexOf(end);
  return text.slice(0, start) + content + text.slice(stop + end.length);
}

async function main() {
  const typesSf = parse(await readText(TYPES), TYPES);
  const slotsSf = parse(await readText(SLOTS_API), SLOTS_API);
  const labelsSf = parse(await readText(SERVICE_LIST), SERVICE_LIST);

  const ifaces = new Map([...collectInterfaces(typesSf), ...collectInterfaces(slotsSf)]);
  const block = findModuleBlock(typesSf, MODULE_NAME);
  const context = findInterface(block, "Context");
  const events = findInterface(block, "Events");

  assertServiceList(
    context.members.map((m) => m.name.getText(typesSf)),
    collectLabelKeys(labelsSf),
  );

  const generated = {
    services: blockContent(
      "services",
      renderTable(["服务", "方法", "说明"], serviceRows(context, ifaces, typesSf)),
    ),
    events: blockContent(
      "events",
      renderTable(["事件", "载荷", "分派", "说明"], eventRows(events, typesSf)),
    ),
  };

  const doc = (await readText(DOC)).replace(/\r\n/g, "\n");

  if (process.argv.includes("--check")) {
    const drift = BLOCKS.filter((name) => extractBlock(doc, name) !== generated[name]);
    if (drift.length) {
      console.error(`ctx API 文档与契约不一致：${drift.join("、")}；运行 pnpm run ctx-api 重新生成。`);
      process.exitCode = 1;
      return;
    }
    console.log("ctx API 文档与契约一致。");
    return;
  }

  let next = doc;
  for (const name of BLOCKS) next = replaceBlock(next, name, generated[name]);
  await writeFile(resolve(ROOT, DOC), next, "utf8");
  console.log(`已更新 ${DOC}。`);
}

await main();
