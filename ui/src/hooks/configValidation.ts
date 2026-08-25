import type { AppConfig, Month, ScheduleConfig } from "./useTauri"

export type ConfigValidationErrors = Partial<Record<keyof AppConfig, string>>

const MAX_INTERVAL_SECONDS = 86_400
const MAX_TRIES = 100_000
const MAX_CONCURRENCY = 16
const MAX_KEEP_ALIVE_MINUTES = 1_440
const MAX_TRIGGER_DELAY_SECONDS = 9_999 * 60 + 59
const INVALID_TASK_NAME_CHARACTERS = '<>:"/\\|?*&^%$;'

const MONTH_MAX_DAY: Record<Month, number> = {
  january: 31,
  february: 29,
  march: 31,
  april: 30,
  may: 31,
  june: 30,
  july: 31,
  august: 31,
  september: 30,
  october: 31,
  november: 30,
  december: 31,
}

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
    !Number.isInteger(config.concurrency) ||
    config.concurrency < 1 ||
    config.concurrency > MAX_CONCURRENCY
  ) {
    errors.concurrency = `并发线程数必须是 1–${MAX_CONCURRENCY} 的整数`
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

  const scheduleError = validateSchedule(config.schedule)
  if (scheduleError) errors.schedule = scheduleError

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

export function validateSchedule(schedule: ScheduleConfig): string | undefined {
  switch (schedule.kind) {
    case "daily":
      if (!isValidTime(schedule.time)) return "每日触发时间必须是严格 HH:mm"
      if (!isIntegerInRange(schedule.everyDays, 1, 365)) {
        return "每日触发间隔必须是 1–365 天的整数"
      }
      return undefined
    case "weekly":
      if (!isValidTime(schedule.time)) return "每周触发时间必须是严格 HH:mm"
      if (!isIntegerInRange(schedule.everyWeeks, 1, 52)) {
        return "每周触发间隔必须是 1–52 周的整数"
      }
      if (schedule.days.length === 0) return "每周触发至少选择一个星期"
      return undefined
    case "monthly": {
      if (!isValidTime(schedule.time)) return "每月触发时间必须是严格 HH:mm"
      if (schedule.day.kind === "day") {
        if (!isIntegerInRange(schedule.day.day, 1, 31)) {
          return "每月触发日期必须是 1–31 的整数"
        }
        const impossible = schedule.months.find(
          (month) => schedule.day.kind === "day" && schedule.day.day > MONTH_MAX_DAY[month],
        )
        if (impossible) return "所选月份不存在该日期；可改用每月最后一天"
      }
      return undefined
    }
    case "interval": {
      if (!isValidTime(schedule.startTime)) return "间隔触发起始时间必须是严格 HH:mm"
      const maximum = schedule.unit === "minutes" ? 1_439 : 23
      if (!isIntegerInRange(schedule.every, 1, maximum)) {
        return `触发间隔必须是 1–${maximum} 的整数`
      }
      return undefined
    }
    case "atLogon":
    case "atStartup":
      if (!isIntegerInRange(schedule.delaySeconds, 0, MAX_TRIGGER_DELAY_SECONDS)) {
        return `触发延迟必须是 0–${MAX_TRIGGER_DELAY_SECONDS} 秒的整数`
      }
      return undefined
    case "cron":
      return validateCron(schedule.expression)
  }
}

function validateCron(expression: string): string | undefined {
  const fields = expression.trim().split(/\s+/).filter(Boolean)
  if (fields.length !== 5) {
    return "Cron 必须是五字段：minute hour day-of-month month day-of-week"
  }
  if (fields.some((field) => /[?LW#]/i.test(field))) {
    return "Cron 暂不支持 L、W、#、? 等扩展语法"
  }
  const [minute, hour, dayOfMonth, month, dayOfWeek] = fields
  const minuteStep = /^\*\/(\d+)$/.exec(minute)
  if (minuteStep) {
    if (!isIntegerInRange(Number(minuteStep[1]), 1, 59)) return "Cron 分钟步长必须是 1–59"
    return [hour, dayOfMonth, month, dayOfWeek].every((field) => field === "*")
      ? undefined
      : "Cron 分钟步长不能与日期限制组合"
  }

  const minuteValue = parseExactCronValue(minute, 0, 59)
  if (minuteValue === undefined) return "Cron minute 必须是 0–59 的单一数值或 */N"
  const hourStep = /^(\*|\d+)\/(\d+)$/.exec(hour)
  if (hourStep) {
    const start = hourStep[1] === "*" ? 0 : Number(hourStep[1])
    const step = Number(hourStep[2])
    if (!isIntegerInRange(start, 0, 23) || !isIntegerInRange(step, 1, 23)) {
      return "Cron 小时起点必须是 0–23，步长必须是 1–23"
    }
    return [dayOfMonth, month, dayOfWeek].every((field) => field === "*")
      ? undefined
      : "Cron 小时步长不能与日期限制组合"
  }

  const hourValue = parseExactCronValue(hour, 0, 23)
  if (hourValue === undefined) return "Cron hour 必须是 0–23 的单一数值或 H/N"
  if (dayOfMonth === "*" && month === "*" && dayOfWeek === "*") return undefined
  if (dayOfMonth === "*" && month === "*" && dayOfWeek !== "*") {
    return validateNamedSet(
      dayOfWeek,
      0,
      7,
      ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"],
    )
      ? undefined
      : "Cron day-of-week 仅支持数字/英文缩写、列表和升序范围"
  }
  if (dayOfMonth !== "*" && dayOfWeek === "*") {
    if (parseExactCronValue(dayOfMonth, 1, 31) === undefined) {
      return "Cron day-of-month 必须是 1–31 的单一数值"
    }
    if (
      month !== "*" &&
      !validateNamedSet(
        month,
        1,
        12,
        ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"],
      )
    ) {
      return "Cron month 仅支持数字/英文缩写、列表和升序范围"
    }
    return undefined
  }
  return "该 Cron 无法无损映射为单个 Windows 触发器"
}

function validateNamedSet(
  value: string,
  minimum: number,
  maximum: number,
  names: string[],
): boolean {
  const parse = (part: string) => {
    const upper = part.toUpperCase()
    const namedIndex = names.indexOf(upper)
    if (namedIndex >= 0) return minimum === 0 ? namedIndex : namedIndex + 1
    const number = Number(part)
    return Number.isInteger(number) && number >= minimum && number <= maximum
      ? number
      : undefined
  }
  return value.split(",").every((part) => {
    const range = part.split("-")
    if (range.length === 1) return parse(range[0]) !== undefined
    if (range.length !== 2) return false
    const start = parse(range[0])
    const end = parse(range[1])
    return start !== undefined && end !== undefined && start <= end
  })
}

function parseExactCronValue(value: string, minimum: number, maximum: number) {
  if (!/^\d+$/.test(value)) return undefined
  const number = Number(value)
  return isIntegerInRange(number, minimum, maximum) ? number : undefined
}

function isIntegerInRange(value: number, minimum: number, maximum: number) {
  return Number.isInteger(value) && value >= minimum && value <= maximum
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
