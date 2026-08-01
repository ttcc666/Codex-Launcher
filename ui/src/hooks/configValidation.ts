import type { AppConfig } from "./useTauri"

export type ConfigValidationErrors = Partial<Record<keyof AppConfig, string>>

const MAX_INTERVAL_SECONDS = 86_400
const MAX_TRIES = 100_000
const MAX_KEEP_ALIVE_MINUTES = 1_440
const INVALID_TASK_NAME_CHARACTERS = '<>:"/\\|?*&^%$;'

export function validateConfig(config: AppConfig): ConfigValidationErrors {
  const errors: ConfigValidationErrors = {}

  if (!config.command.trim()) errors.command = "执行命令不能为空"
  if (!config.workDir.trim()) errors.workDir = "请选择工作目录"
  if (
    !Number.isInteger(config.interval) ||
    config.interval < 1 ||
    config.interval > MAX_INTERVAL_SECONDS
  ) {
    errors.interval = `重试间隔必须是 1–${MAX_INTERVAL_SECONDS} 的整数`
  }
  if (
    !Number.isInteger(config.maxTries) ||
    config.maxTries < 0 ||
    config.maxTries > MAX_TRIES
  ) {
    errors.maxTries = `最大尝试次数必须是 0–${MAX_TRIES} 的整数`
  }
  if (
    !Number.isInteger(config.keepAliveIntervalMinutes) ||
    config.keepAliveIntervalMinutes < 1 ||
    config.keepAliveIntervalMinutes > MAX_KEEP_ALIVE_MINUTES
  ) {
    errors.keepAliveIntervalMinutes = `保活间隔必须是 1–${MAX_KEEP_ALIVE_MINUTES} 分钟的整数`
  }

  const taskName = config.taskName.trim()
  if (!taskName) {
    errors.taskName = "计划任务名称不能为空"
  } else if (taskName.length > 238 || hasInvalidTaskNameCharacter(taskName)) {
    errors.taskName = "计划任务名称过长或包含不允许的字符"
  }

  if (!isValidTime(config.dailyAt)) {
    errors.dailyAt = "请输入 00:00–23:59 的严格 HH:mm 时间"
  }

  const invalidUrl = config.allowedBaseUrls
    .split(/[;；]/)
    .map((value) => value.trim())
    .filter(Boolean)
    .find((value) => !isHttpUrl(value))
  if (invalidUrl) errors.allowedBaseUrls = `无效的 http/https URL：${invalidUrl}`

  return errors
}

export function isConfigValid(config: AppConfig): boolean {
  return Object.keys(validateConfig(config)).length === 0
}

function hasInvalidTaskNameCharacter(value: string): boolean {
  return [...value].some(
    (character) =>
      character.charCodeAt(0) <= 0x1f ||
      INVALID_TASK_NAME_CHARACTERS.includes(character),
  )
}

function isValidTime(value: string): boolean {
  const match = /^(\d{2}):(\d{2})$/.exec(value.trim())
  if (!match) return false
  const hours = Number(match[1])
  const minutes = Number(match[2])
  return hours >= 0 && hours <= 23 && minutes >= 0 && minutes <= 59
}

function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value)
    return (
      (url.protocol === "http:" || url.protocol === "https:") &&
      Boolean(url.hostname) &&
      !url.username &&
      !url.password
    )
  } catch {
    return false
  }
}
