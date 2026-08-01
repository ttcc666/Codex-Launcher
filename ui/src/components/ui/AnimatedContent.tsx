import { useEffect, useRef, useState, type CSSProperties, type HTMLAttributes } from "react"

import { useReducedMotion } from "@/hooks/useReducedMotion"
import { cn } from "@/lib/utils"

interface AnimatedContentProps extends HTMLAttributes<HTMLDivElement> {
  delay?: number
  distance?: number
  duration?: number
  blur?: number
}

export function AnimatedContent({
  children,
  className,
  delay = 0,
  distance = 18,
  duration = 520,
  blur = 4,
  style,
  ...props
}: AnimatedContentProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const prefersReducedMotion = useReducedMotion()
  const [isVisible, setIsVisible] = useState(prefersReducedMotion)

  useEffect(() => {
    if (prefersReducedMotion) {
      setIsVisible(true)
      return
    }

    const container = containerRef.current
    if (!container || !("IntersectionObserver" in window)) {
      setIsVisible(true)
      return
    }

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) return
        setIsVisible(true)
        observer.disconnect()
      },
      { threshold: 0.08 },
    )

    observer.observe(container)
    return () => observer.disconnect()
  }, [prefersReducedMotion])

  const motionStyle: CSSProperties = prefersReducedMotion
    ? {}
    : {
        opacity: isVisible ? 1 : 0,
        transform: isVisible ? "translate3d(0, 0, 0)" : `translate3d(0, ${distance}px, 0)`,
        filter: isVisible ? "blur(0)" : `blur(${blur}px)`,
        transitionProperty: "opacity, transform, filter",
        transitionDuration: `${duration}ms`,
        transitionDelay: `${delay}ms`,
        transitionTimingFunction: "cubic-bezier(0.22, 1, 0.36, 1)",
        willChange: isVisible ? "auto" : "opacity, transform, filter",
      }

  return (
    <div
      ref={containerRef}
      className={cn(className)}
      style={{ ...motionStyle, ...style }}
      {...props}
    >
      {children}
    </div>
  )
}
