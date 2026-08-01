import { useEffect, useMemo, useRef, useState } from "react"

import { useReducedMotion } from "@/hooks/useReducedMotion"
import { cn } from "@/lib/utils"

interface CountUpProps {
  value: number
  duration?: number
  decimals?: number
  className?: string
}

export function CountUp({ value, duration = 520, decimals = 0, className }: CountUpProps) {
  const prefersReducedMotion = useReducedMotion()
  const currentValueRef = useRef(value)
  const [displayValue, setDisplayValue] = useState(value)
  const formatter = useMemo(
    () =>
      new Intl.NumberFormat("zh-CN", {
        minimumFractionDigits: decimals,
        maximumFractionDigits: decimals,
      }),
    [decimals],
  )

  useEffect(() => {
    const from = currentValueRef.current
    if (prefersReducedMotion || duration <= 0 || from === value) {
      currentValueRef.current = value
      setDisplayValue(value)
      return
    }

    let animationFrame = 0
    let startedAt: number | undefined

    const animate = (timestamp: number) => {
      startedAt ??= timestamp
      const progress = Math.min((timestamp - startedAt) / duration, 1)
      const easedProgress = 1 - Math.pow(1 - progress, 3)
      const nextValue = from + (value - from) * easedProgress

      currentValueRef.current = nextValue
      setDisplayValue(nextValue)

      if (progress < 1) {
        animationFrame = window.requestAnimationFrame(animate)
      } else {
        currentValueRef.current = value
        setDisplayValue(value)
      }
    }

    animationFrame = window.requestAnimationFrame(animate)
    return () => window.cancelAnimationFrame(animationFrame)
  }, [duration, prefersReducedMotion, value])

  const visualValue = decimals === 0 ? Math.round(displayValue) : displayValue

  return (
    <span className={cn("tabular-nums", className)}>
      <span aria-hidden="true">{formatter.format(visualValue)}</span>
      <span className="sr-only">{formatter.format(value)}</span>
    </span>
  )
}
