import { useEffect, useMemo, useState, type ReactNode } from "react"

import { isTheme, ThemeContext, type Theme } from "@/components/theme"

interface ThemeProviderProps {
  children: ReactNode
  defaultTheme?: Theme
  storageKey?: string
}

export function ThemeProvider({
  children,
  defaultTheme = "system",
  storageKey = "vite-ui-theme",
}: ThemeProviderProps) {
  const [theme, setThemeState] = useState<Theme>(() => {
    const stored = localStorage.getItem(storageKey)
    return isTheme(stored) ? stored : defaultTheme
  })

  useEffect(() => {
    const root = document.documentElement
    const media = window.matchMedia("(prefers-color-scheme: dark)")
    const applyTheme = () => {
      root.classList.remove("light", "dark")
      root.classList.add(theme === "system" ? (media.matches ? "dark" : "light") : theme)
    }

    applyTheme()
    if (theme !== "system") return
    media.addEventListener("change", applyTheme)
    return () => media.removeEventListener("change", applyTheme)
  }, [theme])

  const value = useMemo(
    () => ({
      theme,
      setTheme: (nextTheme: Theme) => {
        localStorage.setItem(storageKey, nextTheme)
        setThemeState(nextTheme)
      },
    }),
    [storageKey, theme],
  )

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
}
