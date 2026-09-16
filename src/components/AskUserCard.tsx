import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import type { AskRequest } from "~/lib/types";

interface Props {
  ask: () => AskRequest | null;
  onToggleOption: (title: string) => void;
  onSetFreeform: (v: string) => void;
  onSetComment: (v: string) => void;
  onAnswer: (cancelled: boolean) => void;
}

// ── ask_user（pi-ask-user 移动原生化）──
export function AskUserCard(props: Props) {
  return (
    <Show when={props.ask()}>
      {(a) => (
        <div class="approval">
          <div class="approval-title">❓ {a().question}</div>
          <Show when={a().context}>
            <div class="ask-context">{a().context}</div>
          </Show>
          <Show when={a().options.length}>
            <div class="ask-options">
              <For each={a().options}>
                {(o) => (
                  <button
                    type="button"
                    class={`ask-option ${a().selected.includes(o.title) ? "selected" : ""}`}
                    onClick={() => props.onToggleOption(o.title)}
                  >
                    <div class="ask-option-title">
                      <span class="ask-option-mark">
                        {a().selected.includes(o.title) ? "●" : "○"}
                      </span>
                      {o.title}
                    </div>
                    <Show when={o.description}>
                      <div class="ask-option-desc">{o.description}</div>
                    </Show>
                  </button>
                )}
              </For>
            </div>
          </Show>
          <Show when={a().allowFreeform}>
            <input
              class="ask-input"
              placeholder="Or write your own answer…"
              value={a().freeform}
              onInput={(e) => props.onSetFreeform(e.currentTarget.value)}
            />
          </Show>
          <Show when={a().allowComment}>
            <input
              class="ask-input"
              placeholder="Optional comment…"
              value={a().comment}
              onInput={(e) => props.onSetComment(e.currentTarget.value)}
            />
          </Show>
          <div class="approval-actions">
            <Button variant="secondary" onClick={() => props.onAnswer(true)}>
              Skip
            </Button>
            <Button
              onClick={() => props.onAnswer(false)}
              disabled={!a().selected.length && !a().freeform.trim()}
            >
              Answer
            </Button>
          </div>
        </div>
      )}
    </Show>
  );
}
