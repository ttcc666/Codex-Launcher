import type { CSSProperties, HTMLAttributes } from "react"

import { cn } from "@/lib/utils"

interface GradualBlurProps extends HTMLAttributes<HTMLDivElement> {
  position?: "top" | "bottom"
  height?: number
}

const blurLayers = [
  { blur: 1, start: 0, end: 48 },
  { blur: 2, start: 30, end: 78 },
  { blur: 4, start: 58, end: 100 },
]

export function GradualBlur({
  position = "bottom",
  height = 28,
  className,
  style,
  ...props
}: GradualBlurProps) {
  const direction = position === "bottom" ? "to bottom" : "to top"

  return (
    <div
      aria-hidden="true"
      className={cn(
        "pointer-events-none absolute inset-x-0 z-20 overflow-hidden",
        position === "bottom" ? "bottom-0" : "top-0",
        className,
      )}
      style={{ height, ...style }}
      {...props}
    >
      {blurLayers.map((layer) => {
        const maskImage = `linear-gradient(${direction}, transparent ${layer.start}%, black ${layer.end}%)`
        return (
          <div
            key={layer.blur}
            className="absolute inset-0"
            style={
              {
                backdropFilter: `blur(${layer.blur}px)`,
                WebkitBackdropFilter: `blur(${layer.blur}px)`,
                maskImage,
                WebkitMaskImage: maskImage,
              } as CSSProperties
            }
          />
        )
      })}
      <div
        className={cn(
          "absolute inset-0",
          position === "bottom"
            ? "bg-gradient-to-b from-transparent to-zinc-950/55"
            : "bg-gradient-to-t from-transparent to-zinc-950/45",
        )}
      />
    </div>
  )
}
