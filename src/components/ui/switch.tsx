import { cn } from "~/lib/utils";

/**
 * 轻量开关（iOS 形态）：LobeHub 风格管理页的行内启停控件。
 * 手写而非 Kobalte Switch —— 无受控状态依赖，方便与服务端状态同步。
 */
export function ToggleSwitch(props: {
  on: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  class?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.on}
      disabled={props.disabled}
      class={cn("ui-switch", props.on ? "on" : "", props.class)}
      onClick={() => props.onChange(!props.on)}
    >
      <span class="ui-switch-thumb" />
    </button>
  );
}
