import React from "react"
import { cn } from "@/lib/utils"

interface ShinyTextProps {
  text: string
  disabled?: boolean
  speed?: number
  className?: string
}

export function ShinyText({
  text,
  disabled = false,
  speed = 5,
  className = "",
}: ShinyTextProps) {
  const animationDuration = `${speed}s`

  return (
    <span
      className={cn(
        "inline-block bg-clip-text font-bold text-transparent bg-gradient-to-r from-zinc-900 via-emerald-600 to-zinc-900 dark:from-zinc-100 dark:via-emerald-400 dark:to-zinc-100",
        !disabled && "animate-shiny-text",
        className
      )}
      style={{
        animationDuration,
      }}
    >
      {text}
    </span>
  )
}
