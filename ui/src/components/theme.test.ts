import { expect, it } from "vitest"

import { isTheme } from "./theme"

it("accepts only supported stored theme values", () => {
  expect(isTheme("dark")).toBe(true)
  expect(isTheme("light")).toBe(true)
  expect(isTheme("system")).toBe(true)
  expect(isTheme("malicious-class")).toBe(false)
  expect(isTheme(null)).toBe(false)
})
