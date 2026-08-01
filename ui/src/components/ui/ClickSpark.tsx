import {
  useEffect,
  useRef,
  type HTMLAttributes,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react"

import { useReducedMotion } from "@/hooks/useReducedMotion"
import { cn } from "@/lib/utils"

interface Spark {
  x: number
  y: number
  startedAt: number
}

interface ClickSparkProps extends Omit<HTMLAttributes<HTMLDivElement>, "color"> {
  children: ReactNode
  color?: string
  sparkCount?: number
  sparkRadius?: number
  sparkSize?: number
  duration?: number
  disabled?: boolean
}

export function ClickSpark({
  children,
  className,
  color = "#34d399",
  sparkCount = 8,
  sparkRadius = 18,
  sparkSize = 7,
  duration = 380,
  disabled = false,
  onPointerDown,
  ...props
}: ClickSparkProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const sparksRef = useRef<Spark[]>([])
  const animationFrameRef = useRef<number | undefined>(undefined)
  const prefersReducedMotion = useReducedMotion()

  useEffect(
    () => () => {
      if (animationFrameRef.current !== undefined) {
        window.cancelAnimationFrame(animationFrameRef.current)
      }
    },
    [],
  )

  const renderSparks = (timestamp: number) => {
    const container = containerRef.current
    const canvas = canvasRef.current
    if (!container || !canvas) return

    const rect = container.getBoundingClientRect()
    const context = canvas.getContext("2d")
    if (!context) return

    context.clearRect(0, 0, rect.width, rect.height)
    const activeSparks = sparksRef.current.filter(
      (spark) => timestamp - spark.startedAt < duration,
    )

    for (const spark of activeSparks) {
      const progress = Math.min((timestamp - spark.startedAt) / duration, 1)
      const easedProgress = 1 - Math.pow(1 - progress, 3)
      const innerRadius = sparkRadius * easedProgress * 0.35
      const outerRadius = sparkRadius * easedProgress

      context.save()
      context.strokeStyle = color
      context.lineWidth = 1.5
      context.lineCap = "round"
      context.globalAlpha = 1 - progress

      for (let index = 0; index < sparkCount; index += 1) {
        const angle = (Math.PI * 2 * index) / sparkCount
        const cos = Math.cos(angle)
        const sin = Math.sin(angle)
        const tail = sparkSize * (1 - progress)

        context.beginPath()
        context.moveTo(spark.x + cos * innerRadius, spark.y + sin * innerRadius)
        context.lineTo(
          spark.x + cos * (outerRadius + tail),
          spark.y + sin * (outerRadius + tail),
        )
        context.stroke()
      }
      context.restore()
    }

    sparksRef.current = activeSparks
    if (activeSparks.length > 0) {
      animationFrameRef.current = window.requestAnimationFrame(renderSparks)
    } else {
      animationFrameRef.current = undefined
    }
  }

  const handlePointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    onPointerDown?.(event)
    if (disabled || prefersReducedMotion || event.defaultPrevented) return

    const container = containerRef.current
    const canvas = canvasRef.current
    if (!container || !canvas) return

    const rect = container.getBoundingClientRect()
    const devicePixelRatio = Math.min(window.devicePixelRatio || 1, 2)
    canvas.width = Math.max(1, Math.round(rect.width * devicePixelRatio))
    canvas.height = Math.max(1, Math.round(rect.height * devicePixelRatio))
    const context = canvas.getContext("2d")
    context?.setTransform(devicePixelRatio, 0, 0, devicePixelRatio, 0, 0)

    sparksRef.current.push({
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
      startedAt: performance.now(),
    })

    if (animationFrameRef.current === undefined) {
      animationFrameRef.current = window.requestAnimationFrame(renderSparks)
    }
  }

  return (
    <div
      ref={containerRef}
      className={cn("relative isolate inline-flex overflow-visible", className)}
      onPointerDown={handlePointerDown}
      {...props}
    >
      {children}
      <canvas
        ref={canvasRef}
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 z-20 size-full overflow-visible"
      />
    </div>
  )
}
