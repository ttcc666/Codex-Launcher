import type { CSSProperties, HTMLAttributes, ReactNode } from "react"

import { cn } from "@/lib/utils"

export type BorderGlowStatus = "idle" | "starting" | "running" | "success" | "failed" | "stopped"

interface BorderGlowProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode
  status: BorderGlowStatus
}

const statusColors: Record<BorderGlowStatus, string> = {
  idle: "#52525b",
  starting: "#38bdf8",
  running: "#34d399",
  success: "#22c55e",
  failed: "#fb7185",
  stopped: "#fbbf24",
}

export function BorderGlow({ children, status, className, style, ...props }: BorderGlowProps) {
  const isActive = status === "starting" || status === "running"
  const isTerminal = status === "success" || status === "failed" || status === "stopped"
  const glowStyle = {
    "--status-glow-color": statusColors[status],
    ...style,
  } as CSSProperties

  return (
    <div
      className={cn(
        "status-border-glow",
        isActive && "status-border-glow--active",
        isTerminal && "status-border-glow--terminal",
        className,
      )}
      data-status={status}
      style={glowStyle}
      {...props}
    >
      <div className="status-border-glow__content">{children}</div>
    </div>
  )
}
