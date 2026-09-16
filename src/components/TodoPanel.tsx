import { FiCheck, FiCircle, FiLoader, FiX } from "solid-icons/fi";
import { For, Show } from "solid-js";
import type { TodoTask } from "~/lib/types";

interface Props {
  todoOpen: () => boolean;
  todos: () => { tasks: TodoTask[]; nextId: number };
  onClose: () => void;
}

// ── todo 面板（@juicesharp/rpiv-todo 移动原生化）──
// 上游 TUI overlay 的移动形态：列表非空自动显示（todo_updated 事件驱动），
// ✓ 完成 / ◐ 进行中（带 activeForm）/ ○ 待办；墓碑行不上屏。
export function TodoPanel(props: Props) {
  const visibleTodos = () =>
    props.todos().tasks.filter((t) => t.status !== "deleted");
  const todoHeading = () => {
    const all = visibleTodos();
    const done = all.filter((t) => t.status === "completed").length;
    return `Todos (${done}/${all.length})`;
  };
  const todoGlyph = (t: TodoTask) =>
    t.status === "completed" ? (
      <FiCheck size="0.85em" />
    ) : t.status === "in_progress" ? (
      <FiLoader size="0.85em" />
    ) : (
      <FiCircle size="0.85em" />
    );

  return (
    <Show when={props.todoOpen()}>
      <div class="todo-panel">
        <div class="todo-head">
          <span class="todo-title">{todoHeading()}</span>
          <button
            type="button"
            class="goal-btn"
            onClick={props.onClose}
            aria-label="hide todos"
          >
            <FiX size="0.9em" />
          </button>
        </div>
        <Show
          when={visibleTodos().length > 0}
          fallback={
            <div class="todo-row todo-empty">
              No todos yet. Ask the agent to add some!
            </div>
          }
        >
          <For each={visibleTodos()}>
            {(t) => (
              <div class={`todo-row todo-${t.status}`}>
                <span class="todo-glyph">{todoGlyph(t)}</span>
                <span class="todo-subject">
                  #{t.id} {t.subject}
                  <Show when={t.status === "in_progress" && t.activeForm}>
                    <span class="todo-active"> ({t.activeForm})</span>
                  </Show>
                </span>
              </div>
            )}
          </For>
        </Show>
      </div>
    </Show>
  );
}
