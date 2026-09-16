import { FiCopy } from "solid-icons/fi";
import { Show } from "solid-js";
import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "~/components/ui/dialog";
import { copyText } from "~/lib/format";
import type { TreeEntry } from "~/ui/WorkspaceTree";

interface Props {
  preview: () => { path: string; content: string } | null;
  tree: () => TreeEntry[];
  onClose: () => void;
}

export function FilePreviewDialog(props: Props) {
  return (
    <Show when={props.preview()}>
      {(p) => {
        const meta = props.tree().find((t) => t.path === p().path);
        const lines = p().content.length ? p().content.split("\n").length : 0;
        const fmtSize = (n?: number) =>
          n == null
            ? ""
            : n < 1024
              ? `${n} B`
              : n < 1024 * 1024
                ? `${(n / 1024).toFixed(1)} KB`
                : `${(n / 1024 / 1024).toFixed(1)} MB`;
        return (
          <Dialog open={true} onOpenChange={(o) => !o && props.onClose()}>
            <DialogContent class="w-[95vw] max-w-2xl gap-2 p-4">
              <DialogHeader>
                <DialogTitle class="truncate font-mono text-sm">
                  {p().path}
                </DialogTitle>
                <DialogDescription>
                  read-only · {lines} lines
                  {meta ? ` · ${fmtSize(meta.size)}` : ""}
                </DialogDescription>
              </DialogHeader>
              <pre class="preview-body max-h-[65vh] overflow-auto">
                {p().content}
              </pre>
              <div class="flex justify-end gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => copyText(p().content)}
                >
                  <FiCopy size="0.9em" /> Copy
                </Button>
              </div>
            </DialogContent>
          </Dialog>
        );
      }}
    </Show>
  );
}
