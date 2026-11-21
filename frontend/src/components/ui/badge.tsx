import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5 text-[10px] font-medium before:size-1 before:rounded-full before:bg-current",
  {
    variants: {
      variant: {
        default: "bg-accent-soft text-accent",
        muted: "bg-muted text-muted-foreground",
        outline: "border border-border text-muted-foreground before:hidden",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

function Badge({
  className,
  variant,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return (
    <span className={cn(badgeVariants({ variant }), className)} {...props} />
  );
}

export { Badge, badgeVariants };
