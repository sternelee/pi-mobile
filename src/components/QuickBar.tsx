import { FiCheckSquare, FiChevronDown, FiCpu, FiFolder } from "solid-icons/fi";
import { Show } from "solid-js";

interface Props {
  ready: () => boolean;
  onOpenFiles: () => void;
  onOpenModelPicker: () => void;
  modelName: () => string;
  todoOpen: () => boolean;
  onToggleTodos: () => void;
}

// 会话底部快捷条（composer-card 内嵌 chips）：文件 / 模型快选 / Todos
// 小屏横向滚动（渐变 mask 提示还有内容）
export function QuickBar(props: Props) {
  return (
    <Show when={props.ready()}>
      <div class="quick-bar">
        <button type="button" class="quick-chip" onClick={props.onOpenFiles}>
          <FiFolder size="0.95em" /> <span>Files</span>
        </button>
        <button
          type="button"
          class="quick-chip"
          onClick={props.onOpenModelPicker}
        >
          <FiCpu size="0.95em" /> <span>{props.modelName()}</span>
          <span class="quick-caret">
            <FiChevronDown size="0.8em" />
          </span>
        </button>
        <button type="button" class="quick-chip" onClick={props.onToggleTodos}>
          <FiCheckSquare size="0.95em" /> <span>Todos</span>
        </button>
      </div>
    </Show>
  );
}
