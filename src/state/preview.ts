import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";

// D15：预览。编写侧本来就有（write/mkdir jail 在 workspace），这里只解决「看」。
// iframe 指向 Rust 的**独立端口**静态服务（src-tauri/src/preview.rs）——与
// /hostcall 不同源是纵深防御；真正的防线是预览页拿不到 host token。
export function usePreview() {
  const [previewOpen, setPreviewOpen] = createSignal(false);
  const [previewPort, setPreviewPort] = createSignal<number | null>(null);
  const [previewPath, setPreviewPath] = createSignal<string | null>(null);
  const [previewList, setPreviewList] = createSignal<string[]>([]);
  const [previewNonce, setPreviewNonce] = createSignal(0);
  const [previewErr, setPreviewErr] = createSignal<string | null>(null);

  const openPreview = async () => {
    setPreviewErr(null);
    try {
      const port = await invoke<number>("preview_start");
      setPreviewPort(port);
      const list = await invoke<string[]>("preview_targets");
      setPreviewList(list);
      // 没选过就自动挑一个；`index.html` 优先（最常见的入口）
      if (!previewPath() && list.length) {
        setPreviewPath(
          list.find((p) => p.toLowerCase().endsWith("index.html")) ?? list[0],
        );
      }
      setPreviewOpen(true);
    } catch (e) {
      setPreviewErr(String(e));
      setPreviewOpen(true);
    }
  };

  const previewSrc = () => {
    const p = previewPath();
    const port = previewPort();
    if (!p || !port) return null;
    // nonce 强制 iframe 重新加载：agent 刚改过文件时要能看到新版本
    return `http://127.0.0.1:${port}/${p}?v=${previewNonce()}`;
  };

  return {
    previewOpen,
    setPreviewOpen,
    previewPort,
    setPreviewPort,
    previewPath,
    setPreviewPath,
    previewList,
    setPreviewList,
    previewNonce,
    setPreviewNonce,
    previewErr,
    setPreviewErr,
    openPreview,
    previewSrc,
  };
}

export type PreviewState = ReturnType<typeof usePreview>;
