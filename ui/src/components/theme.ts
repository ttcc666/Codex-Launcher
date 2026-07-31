import { createContext, useContext } from "react"

export type Theme = "dark" | "light" | "system"

export function isTheme(value: string | null): value is Theme {
  return value === "dark" || value === "light" || value === "system"
}

export interface ThemeContextValue {
  theme: Theme
  setTheme: (theme: Theme) => void
}

export const ThemeContext = createContext<ThemeContextValue | undefined>(undefined)

export function useTheme(): ThemeContextValue {
  const context = useContext(ThemeContext)
  if (!context) throw new Error("useTheme must be used within a ThemeProvider")
  return context
}
