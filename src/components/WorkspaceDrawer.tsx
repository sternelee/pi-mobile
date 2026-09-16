import { FiRotateCw } from "solid-icons/fi";
import { Show } from "solid-js";
import { Button } from "~/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "~/components/ui/sheet";
import type { TreeEntry } from "~/ui/WorkspaceTree";
import { WorkspaceTree } from "~/ui/WorkspaceTree";

interface Props {
  open: () => boolean;
  onOpenChange: (open: boolean) => void;
  onRefresh: () => void;
  tree: () => TreeEntry[];
  onOpenFile: (path: string) => void;
}

export function WorkspaceDrawer(props: Props) {
  return (
    <Sheet open={props.open()} onOpenChange={props.onOpenChange}>
      <SheetContent
        side="left"
        class="sheet-safe w-4/5 max-w-xs gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
      >
        <SheetHeader>
          <SheetTitle class="text-base">Workspace</SheetTitle>
        </SheetHeader>
        <Button variant="outline" size="sm" onClick={props.onRefresh}>
          <FiRotateCw size="0.9em" /> Refresh
        </Button>
        <Show
          when={props.tree().length}
          fallback={<div class="empty-note">workspace is empty</div>}
        >
          {/* @pierre/trees：虚拟滚动树（自带搜索/文件类型图标），点文件开预览 */}
          <WorkspaceTree entries={props.tree()} onOpenFile={props.onOpenFile} />
        </Show>
      </SheetContent>
    </Sheet>
  );
}
