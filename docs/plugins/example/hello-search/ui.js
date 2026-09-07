/**
 * 示例插件：主线程 UI 平面入口（第三方搜索面板）。
 *
 * 展示「注册自己的视图 kind 与内置搜索并列」模型：registerPanel 注册独立 kind，
 * 与内置「搜索」一同出现在「添加视图」菜单，用户自选、可同时打开。
 * 数据经 facade 的仓库访问方法获得（listFiles/openCanvasFile/openNote/openTable，
 * 与内置搜索面板同一输入面，宿主经 provider 注入）。
 */
const { React, h, registerPanel, listFiles, openCanvasFile, openNote, openTable } =
  window.__atelyxPlugin__.forPlugin("com.example.hello-search");

function HelloSearch() {
  const [query, setQuery] = React.useState("");
  const [files, setFiles] = React.useState(null);

  React.useEffect(() => {
    listFiles().then(setFiles).catch(() => setFiles([]));
  }, []);

  const q = query.trim().toLowerCase();
  const results = (files || []).filter((f) => f.name.toLowerCase().includes(q));

  return h(
    "div",
    { style: { padding: 12, fontFamily: "inherit", color: "var(--text-primary)" } },
    h("div", { style: { fontSize: 12, color: "var(--text-muted)", marginBottom: 8 } },
      "第三方搜索面板（与内置搜索并列，可在视图菜单自选）"),
    h("input", {
      value: query,
      placeholder: "按文件名搜索…",
      onInput: (e) => setQuery(e.target.value),
      style: {
        width: "100%",
        boxSizing: "border-box",
        padding: "4px 8px",
        borderRadius: 4,
        border: "1px solid var(--border)",
        background: "var(--bg-tertiary)",
        color: "var(--text-primary)",
        fontSize: 12,
        outline: "none",
      },
    }),
    h("div", { style: { marginTop: 8 } },
      results.slice(0, 50).map((f) =>
        h(
          "div",
          {
            key: f.path,
            onClick: () => {
              if (f.name.toLowerCase().endsWith(".md")) openNote(f.path, f.name);
              else if (f.name.toLowerCase().endsWith(".atb")) openTable(f.path, f.name);
              else if (f.name.toLowerCase().endsWith(".atlx")) openCanvasFile({ id: f.path, title: f.name, file: f.path, updatedAt: f.updatedAt });
            },
            style: { padding: "4px 6px", borderRadius: 4, cursor: "pointer", fontSize: 12 },
          },
          f.name,
        ),
      ),
    ),
  );
}

// 注册自己的视图 kind：与内置「搜索」并列出现在「添加视图」菜单，用户自选、可同时打开。
registerPanel({ kind: "com.example.hello-search", label: "示例搜索", component: HelloSearch });
