import { describe, expect, it } from "vitest"

import { validateConfig } from "./configValidation"
import type { AppConfig } from "./useTauri"

const validConfig: AppConfig = {
  configVersion: 2,
  command: "echo ok",
  workDir: "C:\\",
  interval: 10,
  maxTries: 0,
  concurrency: 1,
  taskName: "Codex Daily",
  schedule: { kind: "daily", time: "08:40", everyDays: 1 },
  allowedBaseUrls: "https://example.com/API",
  keepAlive: false,
  keepAliveIntervalMinutes: 5,
  desktopNotification: {
    enabled: true,
  },
  serverChan: {
    enabled: false,
  },
  email: {
    enabled: false,
    smtpHost: "",
    smtpPort: 465,
    smtpUsername: "",
    toAddress: "",
  },
  proxy: {
    enabled: false,
    url: "",
  },
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

  it("bounds the concurrency worker count to the backend range", () => {
    for (const concurrency of [0, -1, 17, 1.5, Number.NaN]) {
      expect(
        validateConfig({ ...validConfig, concurrency }).concurrency,
      ).toBeTruthy()
    }
    for (const concurrency of [1, 8, 16]) {
      expect(
        validateConfig({ ...validConfig, concurrency }).concurrency,
      ).toBeUndefined()
    }
  })

  it("rejects invalid scheduler time and shell metacharacters", () => {
    expect(
      validateConfig({
        ...validConfig,
        schedule: { kind: "daily", time: "24:00", everyDays: 1 },
      }).schedule,
    ).toBeTruthy()
    expect(
      validateConfig({ ...validConfig, taskName: "task & whoami" }).taskName,
    ).toBeTruthy()
  })

  it("validates advanced triggers and the supported cron subset", () => {
    expect(
      validateConfig({
        ...validConfig,
        schedule: {
          kind: "weekly",
          time: "09:30",
          everyWeeks: 1,
          days: [],
        },
      }).schedule,
    ).toBeTruthy()
    expect(
      validateConfig({
        ...validConfig,
        schedule: { kind: "cron", expression: "30 9 * * MON-FRI" },
      }).schedule,
    ).toBeUndefined()
    expect(
      validateConfig({
        ...validConfig,
        schedule: { kind: "cron", expression: "0,30 9 * * *" },
      }).schedule,
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

  it("keeps Server酱 SendKey outside the autosaved config model", () => {
    const serialized = JSON.stringify({
      ...validConfig,
      serverChan: { ...validConfig.serverChan, enabled: true },
    })

    expect(serialized).toContain('"serverChan"')
    expect(serialized.toLowerCase()).not.toContain("sendkey")
    expect(serialized.toLowerCase()).not.toContain("send_key")
  })

  it("accepts valid proxy URLs when enabled and rejects invalid ones", () => {
    for (const url of [
      "http://127.0.0.1:7890",
      "https://proxy.example.com:8080",
      "socks5://127.0.0.1:1080",
      "socks5h://127.0.0.1:1080",
      "http://user:pass@127.0.0.1:7890",
    ]) {
      expect(
        validateConfig({ ...validConfig, proxy: { enabled: true, url } }).proxy,
      ).toBeUndefined()
    }

    for (const url of ["", "ftp://127.0.0.1:7890", "127.0.0.1:7890", "not-a-url"]) {
      expect(
        validateConfig({ ...validConfig, proxy: { enabled: true, url } }).proxy,
      ).toBeTruthy()
    }
  })

  it("allows an empty proxy URL when the proxy is disabled", () => {
    expect(
      validateConfig({ ...validConfig, proxy: { enabled: false, url: "" } }).proxy,
    ).toBeUndefined()
  })
})
