import { Tabs as TabsPrimitive } from "@base-ui/react/tabs"
import { cva, type VariantProps } from "class-variance-authority"

import { cn } from "@/lib/utils"

function Tabs({
  className,
  orientation = "horizontal",
  ...props
}: TabsPrimitive.Root.Props) {
  return (
    <TabsPrimitive.Root
      data-slot="tabs"
      data-orientation={orientation}
      className={cn(
        "group/tabs flex flex-col gap-2",
        className
      )}
      {...props}
    />
  )
}

const tabsListVariants = cva(
  "group/tabs-list inline-flex h-10 w-fit items-center justify-center rounded-xl p-1 text-muted-foreground bg-zinc-200/80 dark:bg-zinc-900/90 border border-zinc-300/80 dark:border-zinc-800/80 shadow-inner backdrop-blur",
  {
    variants: {
      variant: {
        default: "",
        line: "gap-1 bg-transparent border-none shadow-none",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
)

function TabsList({
  className,
  variant = "default",
  ...props
}: TabsPrimitive.List.Props & VariantProps<typeof tabsListVariants>) {
  return (
    <TabsPrimitive.List
      data-slot="tabs-list"
      data-variant={variant}
      className={cn(tabsListVariants({ variant }), className)}
      {...props}
    />
  )
}

function TabsTrigger({ className, ...props }: TabsPrimitive.Tab.Props) {
  return (
    <TabsPrimitive.Tab
      data-slot="tabs-trigger"
      className={cn(
        "relative inline-flex h-full flex-1 items-center justify-center gap-1.5 rounded-lg px-3 py-1 text-xs font-medium whitespace-nowrap select-none transition-all duration-200",
        "text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100",
        "data-active:bg-white data-active:text-emerald-700 dark:data-active:bg-zinc-800 dark:data-active:text-emerald-400 data-active:font-semibold data-active:shadow-sm border border-transparent data-active:border-zinc-200/90 dark:data-active:border-zinc-700/60",
        "focus-visible:ring-2 focus-visible:ring-emerald-500/50 outline-none active:scale-95",
        className
      )}
      {...props}
    />
  )
}

function TabsContent({ className, ...props }: TabsPrimitive.Panel.Props) {
  return (
    <TabsPrimitive.Panel
      data-slot="tabs-content"
      className={cn(
        "flex-1 text-sm outline-none animate-in fade-in-50 slide-in-from-bottom-1 duration-200",
        className
      )}
      {...props}
    />
  )
}

export { Tabs, TabsList, TabsTrigger, TabsContent }
