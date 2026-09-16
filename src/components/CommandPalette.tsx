import { For, Show } from "solid-js";

interface Props {
  visible: () => boolean;
  commands: () => { cmd: string; desc: string }[];
  onPick: (cmd: string) => void;
}

export function CommandPalette(props: Props) {
  return (
    <Show when={props.visible()}>
      <div class="cmd-palette">
        <For each={props.commands()}>
          {(c) => (
            <button
              type="button"
              class="cmd-row"
              onClick={() => props.onPick(c.cmd)}
            >
              <span class="cmd-name">{c.cmd}</span>
              <span class="cmd-desc">{c.desc}</span>
            </button>
          )}
        </For>
      </div>
    </Show>
  );
}
