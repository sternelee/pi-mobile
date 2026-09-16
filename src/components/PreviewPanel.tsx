import { FiExternalLink, FiRefreshCw, FiX } from "solid-icons/fi";
import { For, Show } from "solid-js";

interface Props {
  open: () => boolean;
  previewPath: () => string | null;
  previewList: () => string[];
  previewSrc: () => string | null;
  previewErr: () => string | null;
  onPickPath: (path: string | null) => void;
  onReload: () => void;
  onOpenExternal: () => void;
  onClose: () => void;
}

// D15 预览。

// 刻意做成一整块可辨认的面板（标题 + 当前路径 + 可点击的关闭），而不是
// 把 iframe 塞进聊天流：预览里跑的是 **agent（LLM）写出来的 JS**，
// 如果它看起来像 app 自己的 UI，那就是一个现成的钓鱼面。

// sandbox 只给 allow-scripts + allow-forms：
// * 不给 allow-same-origin → 预览页是 opaque origin，够不到 app 的 DOM
// * 不给 allow-top-navigation → 不能把 app 导航走
// * 不给 allow-popups → 不能开新窗口
// 网络是**故意**放开的（用户选择，见 D15）——那条风险本模块不拦，
// 能外泄的只有页面自己能生成的、或先经审批写进 workspace 的东西。
export function PreviewPanel(props: Props) {
  return (
    <Show when={props.open()}>
      <div class="preview-sheet">
        <div class="preview-bar">
          <span class="preview-badge">PREVIEW</span>
          <select
            class="preview-pick"
            value={props.previewPath() ?? ""}
            onChange={(e) => props.onPickPath(e.currentTarget.value || null)}
          >
            <Show when={!props.previewList().length}>
              <option value="">no .html in workspace</option>
            </Show>
            <For each={props.previewList()}>
              {(p) => <option value={p}>{p}</option>}
            </For>
          </select>
          <button
            type="button"
            class="preview-btn"
            onClick={props.onReload}
            aria-label="reload"
          >
            <FiRefreshCw size="0.95em" />
          </button>
          {/* A3 逃生口：预览页里的同步死循环会冻住**整个 app**（iframe 与 app
              共用 WebView 主线程，真机实测确认）。系统浏览器是独立进程，
              页面再重也带不倒 app。
              ⚠️ 它救不了已经卡死的现场（那时这个按钮也点不动），是**事前选择**。 */}
          <button
            type="button"
            class="preview-btn"
            onClick={props.onOpenExternal}
            aria-label="open in system browser"
            title="open in system browser (safe if the page hangs)"
          >
            <FiExternalLink size="0.95em" />
          </button>
          <button
            type="button"
            class="preview-btn"
            onClick={props.onClose}
            aria-label="close preview"
          >
            <FiX size="0.95em" />
          </button>
        </div>
        <Show when={props.previewErr()}>
          <div class="preview-err">{props.previewErr()}</div>
        </Show>
        <Show
          when={props.previewSrc()}
          fallback={
            <div class="preview-err">
              nothing to preview yet — ask the agent to write an .html file into
              the workspace
            </div>
          }
        >
          <iframe
            class="preview-frame"
            title="preview"
            src={props.previewSrc()!}
            sandbox="allow-scripts allow-forms"
          />
        </Show>
      </div>
    </Show>
  );
}
