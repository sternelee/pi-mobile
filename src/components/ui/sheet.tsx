import type { Component, ComponentProps, JSX, ValidComponent } from "solid-js";
import { splitProps } from "solid-js";
import * as DialogPrimitive from "@kobalte/core/dialog";
import type {
  DialogDescriptionProps,
  DialogTitleProps,
} from "@kobalte/core/dialog";
import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import type { VariantProps } from "class-variance-authority";
import { cva } from "class-variance-authority";
import { cn } from "~/lib/utils";

const Sheet = DialogPrimitive.Root;
const SheetTrigger = DialogPrimitive.Trigger;
const SheetClose = DialogPrimitive.CloseButton;

const sheetVariants = cva(
  "fixed z-50 flex flex-col gap-3 bg-background shadow-lg transition ease-in-out data-[expanded]:duration-300 data-[closed]:duration-200 data-[expanded]:animate-in data-[closed]:animate-out",
  {
    variants: {
      side: {
        top: "inset-x-0 top-0 border-b data-[closed]:slide-out-to-top data-[expanded]:slide-in-from-top",
        bottom:
          "inset-x-0 bottom-0 border-t data-[closed]:slide-out-to-bottom data-[expanded]:slide-in-from-bottom",
        left: "inset-y-0 left-0 h-full w-4/5 max-w-sm border-r data-[closed]:slide-out-to-left data-[expanded]:slide-in-from-left",
        right:
          "inset-y-0 right-0 h-full w-4/5 max-w-sm border-l data-[closed]:slide-out-to-right data-[expanded]:slide-in-from-right",
      },
    },
    defaultVariants: {
      side: "right",
    },
  },
);

type SheetOverlayProps = { class?: string };

// Kobalte 泛型 JSX 类型在包装组件里推导困难，这里在组件边界收敛类型，
// 调用侧仍获得精确的 props 类型。
const OverlayEl = DialogPrimitive.Overlay as unknown as Component<
  SheetOverlayProps & Record<string, unknown>
>;
const ContentEl = DialogPrimitive.Content as unknown as Component<
  SheetContentProps & Record<string, unknown>
>;

const SheetOverlay = (props: SheetOverlayProps & Record<string, unknown>) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <OverlayEl
      class={cn(
        "fixed inset-0 z-50 bg-black/60 data-[expanded]:animate-in data-[closed]:animate-out data-[closed]:fade-out-0 data-[expanded]:fade-in-0",
        local.class,
      )}
      {...others}
    />
  );
};

type SheetContentProps = VariantProps<typeof sheetVariants> & {
  class?: string;
  children?: JSX.Element;
};

const SheetContent = (props: SheetContentProps & Record<string, unknown>) => {
  const [local, others] = splitProps(props, ["side", "class", "children"]);
  return (
    <DialogPrimitive.Portal>
      <SheetOverlay />
      <ContentEl
        class={cn(sheetVariants({ side: local.side }), local.class)}
        {...others}
      >
        {local.children}
        {/* 关闭按钮跟随状态栏安全距离（WebView 全屏 edge-to-edge） */}
        <DialogPrimitive.CloseButton
          class="absolute right-3 rounded-md p-1 text-muted-foreground opacity-70 transition-opacity hover:opacity-100 focus:outline-none disabled:pointer-events-none"
          style={{ top: "calc(0.75rem + env(safe-area-inset-top, 0px))" }}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="size-4"
          >
            <path d="M18 6 6 18" />
            <path d="m6 6 12 12" />
          </svg>
          <span class="sr-only">Close</span>
        </DialogPrimitive.CloseButton>
      </ContentEl>
    </DialogPrimitive.Portal>
  );
};

const SheetHeader: Component<ComponentProps<"div">> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      class={cn("flex flex-col space-y-1.5 text-left", local.class)}
      {...others}
    />
  );
};

const SheetFooter: Component<ComponentProps<"div">> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      class={cn(
        "flex flex-col-reverse sm:flex-row sm:justify-end sm:space-x-2",
        local.class,
      )}
      {...others}
    />
  );
};

type SheetTitleProps<T extends ValidComponent = "h2"> = DialogTitleProps<T> & {
  class?: string;
};

const SheetTitle = <T extends ValidComponent = "h2">(
  props: PolymorphicProps<T, SheetTitleProps<T>>,
) => {
  const [local, others] = splitProps(props as SheetTitleProps<T>, ["class"]);
  return (
    <DialogPrimitive.Title
      class={cn("text-lg font-semibold text-foreground", local.class)}
      {...others}
    />
  );
};

type SheetDescriptionProps<T extends ValidComponent = "p"> =
  DialogDescriptionProps<T> & { class?: string };

const SheetDescription = <T extends ValidComponent = "p">(
  props: PolymorphicProps<T, SheetDescriptionProps<T>>,
) => {
  const [local, others] = splitProps(props as SheetDescriptionProps<T>, [
    "class",
  ]);
  return (
    <DialogPrimitive.Description
      class={cn("text-sm text-muted-foreground", local.class)}
      {...others}
    />
  );
};

export {
  Sheet,
  SheetTrigger,
  SheetClose,
  SheetContent,
  SheetHeader,
  SheetFooter,
  SheetTitle,
  SheetDescription,
};
