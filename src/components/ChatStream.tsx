import { createScrollPosition } from "@solid-primitives/scroll";
import {
  FiArrowDown,
  FiCheck,
  FiChevronDown,
  FiChevronRight,
  FiLoader,
} from "solid-icons/fi";
import { createEffect, For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "~/components/ui/collapsible";
import { copyText, fmtDur, prettyArgs, toolState } from "~/lib/format";
import { SUGGESTIONS } from "~/lib/providers";
import type { ChatItem } from "~/lib/types";
import { Markdown } from "~/ui/Markdown";

interface ToolCardProps {
  it: ChatItem;
  updateItem: (toolCallId: string, patch: Partial<ChatItem>) => void;
  revert: (it: ChatItem) => void;
}

// 工具卡（agent-workflow-ios 设计稿）：左状态圆图标 + 「TOOL CALL」微标签 +
// mono 工具名摘要；右状态文本（success · 1.6s）+ 展开 chevron
function ToolCard(props: ToolCardProps) {
  const it = () => props.it;
  const state = () => toolState(it());
  const statusText = () => {
    if (it().pending) return "running";
    const label = it().isError ? "failed" : "success";
    const durationMs = it().durationMs;
    return durationMs != null ? `${label} · ${fmtDur(durationMs)}` : label;
  };
  return (
    <Collapsible
      open={it().expanded}
      onOpenChange={(o) => props.updateItem(it().toolCallId!, { expanded: o })}
      class={`tool-card ${state()}`}
    >
      <CollapsibleTrigger class="tool-head">
        <span class="tool-icon" aria-hidden="true">
          <Show
            when={!it().pending}
            fallback={<FiLoader class="tool-spin" size="0.85em" />}
          >
            <Show when={it().isError} fallback={<FiCheck size="0.85em" />}>
              <span class="tool-bang">!</span>
            </Show>
          </Show>
        </span>
        <span class="tool-text">
          <span class="tool-eyebrow">TOOL CALL</span>
          <span class="tool-summary">
            {it().toolName}({(it().argsText ?? "").slice(0, 90)}
            {it().pending ? " …" : ""}
          </span>
        </span>
        <span class="tool-status">{statusText()}</span>
        <span class="tool-caret">
          {it().expanded ? <FiChevronDown /> : <FiChevronRight />}
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div class="tool-body">
          <div>{prettyArgs(it().argsText)}</div>
          <Show when={it().text}>
            <div class="tool-result">{it().text}</div>
          </Show>
          <Show when={it().path && (it().canRevert || it().reverted)}>
            <div class="tool-revert-row">
              <Show
                when={it().canRevert}
                fallback={<span class="reverted-note">↩ reverted</span>}
              >
                <Button
                  variant="secondary"
                  size="sm"
                  class="h-7 rounded-full text-xs"
                  onClick={() => props.revert(it())}
                >
                  ↩ Revert
                </Button>
              </Show>
            </div>
          </Show>
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}

interface Props {
  items: () => ChatItem[];
  ready: () => boolean;
  stick: () => boolean;
  setStick: (v: boolean) => void;
  reducedMotion: () => boolean;
  sendText: (raw: string) => void;
  updateItem: (toolCallId: string, patch: Partial<ChatItem>) => void;
  revert: (it: ChatItem) => void;
}

export function ChatStream(props: Props) {
  const hasConversation = () =>
    props
      .items()
      .some(
        (i) => i.role === "user" || i.role === "assistant" || i.role === "tool",
      );

  let chatEl: HTMLDivElement | undefined;
  // 滚动位置响应式跟踪（@solid-primitives/scroll）——滚离底部 >240px 时浮出跳底按钮
  const chatScroll = createScrollPosition(() => chatEl);
  const awayFromBottom = () => {
    const el = chatEl;
    if (!el) return 0;
    return el.scrollHeight - chatScroll.y - el.clientHeight;
  };
  const jumpToLatest = () => {
    chatEl?.scrollTo({
      top: chatEl.scrollHeight,
      behavior: props.reducedMotion() ? "auto" : "smooth",
    });
    props.setStick(true);
  };
  const onChatScroll = () => {
    if (!chatEl) return;
    props.setStick(
      chatEl.scrollHeight - chatEl.scrollTop - chatEl.clientHeight < 90,
    );
  };

  // ── 自动滚动：贴底跟随，用户上翻即暂停 ──
  createEffect(() => {
    props.items();
    if (chatEl && props.stick()) {
      chatEl.scrollTop = chatEl.scrollHeight;
    }
  });

  return (
    <>
      <div class="chat" ref={chatEl} onScroll={onChatScroll}>
        <Show when={props.ready() && !hasConversation()}>
          <div class="welcome">
            <div class="welcome-logo">π</div>
            <h2>Your pocket coding agent</h2>
            <p>
              pi runs entirely on this device — it can list, read, write and
              edit files in the sandboxed workspace. Writes ask for your
              approval.
            </p>
            <div class="chips">
              <For each={SUGGESTIONS}>
                {(s) => (
                  <Button
                    variant="outline"
                    size="sm"
                    class="rounded-full"
                    onClick={() => props.sendText(s)}
                  >
                    {s}
                  </Button>
                )}
              </For>
            </div>
          </div>
        </Show>

        <For each={props.items()}>
          {(it) => (
            <Show
              when={it.role !== "tool" || it.toolCallId}
              fallback={<div class="result-line">{it.text}</div>}
            >
              <div class={`msg msg-${it.role}`}>
                <Show
                  when={it.role === "tool"}
                  fallback={
                    <Show
                      when={it.role === "assistant"}
                      fallback={
                        <Show
                          when={it.role === "user"}
                          fallback={<span class="status-line">{it.text}</span>}
                        >
                          <div class="bubble">{it.text}</div>
                        </Show>
                      }
                    >
                      <div class={`bubble ${it.thinking ? "thinking" : ""}`}>
                        <Show
                          when={!it.thinking}
                          fallback={
                            <span class="thinking-dots">
                              thinking
                              <span class="dot">.</span>
                              <span class="dot">.</span>
                              <span class="dot">.</span>
                            </span>
                          }
                        >
                          <Markdown text={it.text} streaming={it.streaming} />
                        </Show>
                        <Show when={it.text && !it.thinking}>
                          <button
                            type="button"
                            class="copy-btn"
                            onClick={() => copyText(it.text)}
                            aria-label="copy"
                          >
                            copy
                          </button>
                        </Show>
                      </div>
                    </Show>
                  }
                >
                  <ToolCard
                    it={it}
                    updateItem={props.updateItem}
                    revert={props.revert}
                  />
                </Show>
              </div>
            </Show>
          )}
        </For>
      </div>

      <Show when={awayFromBottom() > 240}>
        <button
          type="button"
          class="jump-btn"
          onClick={jumpToLatest}
          aria-label="jump to latest"
        >
          <FiArrowDown size="0.9em" /> latest
        </button>
      </Show>
    </>
  );
}
