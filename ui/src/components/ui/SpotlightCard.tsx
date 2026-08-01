import React, { useRef, type CSSProperties } from "react"
import { cn } from "@/lib/utils"

interface SpotlightCardProps extends React.HTMLAttributes<HTMLDivElement> {
  spotlightColor?: string
  children: React.ReactNode
}

export function SpotlightCard({
  children,
  className = "",
  spotlightColor = "rgba(16, 185, 129, 0.12)",
  onPointerMove,
  style,
  ...props
}: SpotlightCardProps) {
  const divRef = useRef<HTMLDivElement>(null)

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    onPointerMove?.(event)
    if (!divRef.current || event.pointerType === "touch") return

    const rect = divRef.current.getBoundingClientRect()
    divRef.current.style.setProperty("--spotlight-x", `${event.clientX - rect.left}px`)
    divRef.current.style.setProperty("--spotlight-y", `${event.clientY - rect.top}px`)
  }

  return (
    <div
      ref={divRef}
      onPointerMove={handlePointerMove}
      className={cn(
        "spotlight-card group/spotlight relative overflow-hidden rounded-xl border border-zinc-200/80 bg-card/90 backdrop-blur transition-all duration-300 hover:-translate-y-1 hover:border-emerald-500/40 hover:shadow-xl hover:shadow-emerald-500/5 dark:border-zinc-800",
        className,
      )}
      style={
        {
          "--spotlight-x": "50%",
          "--spotlight-y": "50%",
          ...style,
        } as CSSProperties
      }
      {...props}
    >
      <div
        className="pointer-events-none absolute -inset-px opacity-0 transition-opacity duration-300 group-hover/spotlight:opacity-100 group-focus-within/spotlight:opacity-100"
        style={{
          background: `radial-gradient(460px circle at var(--spotlight-x) var(--spotlight-y), ${spotlightColor}, transparent 42%)`,
        }}
      />
      <div className="relative z-10 h-full">{children}</div>
    </div>
  )
}
