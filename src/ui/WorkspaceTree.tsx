// WorkspaceTree —— @pierre/trees 的 SolidJS 封装（vanilla 入口）。
//
// 文档（https://trees.software/docs）明确：非 React 框架用 vanilla 类，并
// 「create/own the FileTree instance from that framework's lifecycle」——本组件
// 在 onMount 建模型、render 到宿主 div，onCleanup unmount+cleanUp。
// 树状态（选择/展开/搜索）全在模型上，不经 DOM 读取。
//
// 路径约定：目录以 "/" 结尾（FileTreeController 以此识别目录——空目录也
// 不会丢）。workspace_tree 的 kind 字段在这里补上尾斜杠。
import { createEffect, on, onCleanup, onMount, type Component } from "solid-js";
import { FileTree } from "@pierre/trees";

export type TreeEntry = {
  path: string;
  kind: "file" | "directory";
  size: number;
  mtimeMs: number;
};

const toPaths = (entries: readonly TreeEntry[]): string[] =>
  entries.map((e) => (e.kind === "directory" ? `${e.path}/` : e.path));

export const WorkspaceTree: Component<{
  entries: TreeEntry[];
  onOpenFile: (path: string) => void;
}> = (props) => {
  let host!: HTMLDivElement;
  let ft: FileTree | null = null;

  onMount(() => {
    ft = new FileTree({
      paths: toPaths(props.entries),
      search: true,
      fileTreeSearchMode: "hide-non-matches",
      icons: "standard",
      density: "compact",
      onSelectionChange: (selected) => {
        if (!ft) return;
        // 事件回调里现取（非响应式追踪域）：Refresh 后新文件同样可预览
        const files = new Set(
          props.entries.filter((e) => e.kind === "file").map((e) => e.path),
        );
        for (const p of selected) {
          const path = p.endsWith("/") ? p.slice(0, -1) : p;
          if (files.has(path)) props.onOpenFile(path);
        }
        // 打开即反选：同一文件可再次点按（selection change 才触发预览）
        for (const p of selected) ft.getItem(p)?.deselect();
      },
    });
    ft.render({ fileTreeContainer: host });
  });

  // 外部刷新（Refresh / 重新打开 sheet）：resetPaths 复用模型，不重建
  createEffect(
    on(
      () => props.entries,
      (entries) => ft?.resetPaths(toPaths(entries)),
      { defer: true },
    ),
  );

  onCleanup(() => {
    ft?.unmount();
    ft?.cleanUp();
    ft = null;
  });

  return <div ref={host} class="workspace-tree-host" />;
};
