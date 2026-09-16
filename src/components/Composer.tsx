import { FiSend, FiSquare } from "solid-icons/fi";
import { Show } from "solid-js";

interface Props {
  input: () => string;
  setInput: (v: string) => void;
  ready: () => boolean;
  busy: () => boolean;
  onSend: (raw: string) => void;
  onStop: () => void;
  /** 把 textarea 元素注册给 App（sendText 清内容后要复位高度） */
  registerTextarea: (el: HTMLTextAreaElement) => void;
}

export function Composer(props: Props) {
  let textareaEl: HTMLTextAreaElement | undefined;

  const onSubmit = (e: Event) => {
    e.preventDefault();
    props.onSend(props.input());
  };

  const onKeydown = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey && !(e as any).isComposing) {
      e.preventDefault();
      props.onSend(props.input());
    }
  };

  const autoGrow = () => {
    if (!textareaEl) return;
    textareaEl.style.height = "auto";
    textareaEl.style.height = `${Math.min(textareaEl.scrollHeight, 132)}px`;
  };

  return (
    <form class="composer" onSubmit={onSubmit}>
      <div class="composer-pill">
        <textarea
          ref={(el) => {
            textareaEl = el;
            props.registerTextarea(el);
          }}
          rows="1"
          placeholder={
            props.ready()
              ? props.busy()
                ? "Queue a message while pi works…"
                : "Ask pi to do something…"
              : "agent booting…"
          }
          disabled={!props.ready()}
          value={props.input()}
          onInput={(e) => {
            props.setInput(e.currentTarget.value);
            autoGrow();
          }}
          onKeyDown={onKeydown}
        />
        <Show
          when={props.busy() && !props.input().trim()}
          fallback={
            <button
              type="submit"
              class="send-btn"
              disabled={!props.ready() || !props.input().trim()}
              aria-label="send"
            >
              <FiSend size="1em" />
            </button>
          }
        >
          {/* 仅在「空内容 + 响应中」显示停止；有内容时始终显示发送（消息入队） */}
          <button
            type="button"
            class="stop-btn"
            onClick={props.onStop}
            aria-label="stop"
          >
            <FiSquare size="0.95em" />
          </button>
        </Show>
      </div>
    </form>
  );
}
