import * as React from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";

function Dialog({
  open,
  onOpenChange,
  preventClose = false,
  className,
  children,
  "aria-labelledby": labelledby,
  "aria-describedby": describedby,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  preventClose?: boolean;
  className?: string;
  children: React.ReactNode;
  "aria-labelledby"?: string;
  "aria-describedby"?: string;
}) {
  const ref = React.useRef<HTMLDialogElement>(null);
  const closeRef = React.useRef(onOpenChange);
  closeRef.current = onOpenChange;

  React.useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (open && !el.open) el.showModal();
    if (!open && el.open) el.close();
  }, [open]);

  React.useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onClose = () => closeRef.current(false);
    el.addEventListener("close", onClose);
    return () => el.removeEventListener("close", onClose);
  }, []);

  return (
    <dialog
      ref={ref}
      aria-labelledby={labelledby}
      aria-describedby={describedby}
      onCancel={(event) => {
        if (preventClose) event.preventDefault();
      }}
      onClick={(e) => {
        if (preventClose || e.target !== ref.current) return;
        const rect = ref.current.getBoundingClientRect();
        if (
          e.clientX < rect.left ||
          e.clientX > rect.right ||
          e.clientY < rect.top ||
          e.clientY > rect.bottom
        )
          onOpenChange(false);
      }}
      style={{ width: "min(490px, calc(100% - 32px))" }}
      className={cn(
        "m-auto max-h-[calc(100svh-32px)] overflow-y-auto rounded-md border bg-card p-7 text-card-foreground",
        className,
      )}
    >
      {children}
    </dialog>
  );
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("flex flex-col gap-2", className)} {...props} />;
}

function DialogTitle({ className, ...props }: React.ComponentProps<"h2">) {
  return (
    <h2
      className={cn("text-[21px] font-medium tracking-tight", className)}
      {...props}
    />
  );
}

function DialogDescription({ className, ...props }: React.ComponentProps<"p">) {
  return (
    <p
      className={cn("text-[13px] text-muted-foreground", className)}
      {...props}
    />
  );
}

function DialogClose({ className, ...props }: React.ComponentProps<"button">) {
  return (
    <button
      type="button"
      aria-label="Close"
      className={cn(
        "absolute top-4 right-4 rounded-sm p-1 text-muted-foreground transition-colors hover:text-foreground",
        className,
      )}
      {...props}
    >
      <X className="size-4" />
    </button>
  );
}

function DialogFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn("mt-6 flex flex-wrap justify-end gap-2.5", className)}
      {...props}
    />
  );
}

export {
  Dialog,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogClose,
  DialogFooter,
};
