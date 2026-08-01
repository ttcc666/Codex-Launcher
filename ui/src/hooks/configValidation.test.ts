import { describe, expect, it } from "vitest"

import { validateConfig } from "./configValidation"
import type { AppConfig } from "./useTauri"

const validConfig: AppConfig = {
  configVersion: 1,
  command: "echo ok",
  workDir: "C:\\",
  interval: 10,
  maxTries: 0,
  taskName: "Codex Daily",
  dailyAt: "08:40",
  allowedBaseUrls: "https://example.com/API",
  keepAlive: false,
  keepAliveIntervalMinutes: 5,
}

describe("config validation", () => {
  it("rejects invalid numeric values before invoking the backend", () => {
    expect(validateConfig({ ...validConfig, interval: 0 }).interval).toBeTruthy()
    expect(validateConfig({ ...validConfig, maxTries: -1 }).maxTries).toBeTruthy()
    expect(
      validateConfig({ ...validConfig, interval: Number.NaN }).interval,
    ).toBeTruthy()
    expect(
      validateConfig({ ...validConfig, keepAliveIntervalMinutes: 0 })
        .keepAliveIntervalMinutes,
    ).toBeTruthy()
  })

  it("rejects invalid scheduler time and shell metacharacters", () => {
    expect(validateConfig({ ...validConfig, dailyAt: "24:00" }).dailyAt).toBeTruthy()
    expect(
      validateConfig({ ...validConfig, taskName: "task & whoami" }).taskName,
    ).toBeTruthy()
  })

  it("accepts a valid configuration", () => {
    expect(validateConfig(validConfig)).toEqual({})
  })

  it("rejects base URLs containing credentials like the backend", () => {
    expect(
      validateConfig({
        ...validConfig,
        allowedBaseUrls: "https://user:secret@example.com/api",
      }).allowedBaseUrls,
    ).toBeTruthy()
  })
})
