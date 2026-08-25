import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import type { Month, ScheduleConfig, Weekday } from "@/hooks/useTauri"

interface ScheduleEditorProps {
  value: ScheduleConfig
  error?: string
  onChange: (value: ScheduleConfig) => void
}

type ScheduleKind = ScheduleConfig["kind"]

const TRIGGER_TYPES: Array<{ label: string; value: ScheduleKind }> = [
  { label: "每天", value: "daily" },
  { label: "每周", value: "weekly" },
  { label: "每月", value: "monthly" },
  { label: "分钟 / 小时间隔", value: "interval" },
  { label: "用户登录时", value: "atLogon" },
  { label: "Windows 启动时", value: "atStartup" },
  { label: "Cron 表达式", value: "cron" },
]

const WEEKDAYS: Array<{ label: string; value: Weekday }> = [
  { label: "一", value: "monday" },
  { label: "二", value: "tuesday" },
  { label: "三", value: "wednesday" },
  { label: "四", value: "thursday" },
  { label: "五", value: "friday" },
  { label: "六", value: "saturday" },
  { label: "日", value: "sunday" },
]

const MONTHS: Array<{ label: string; value: Month }> = [
  { label: "1月", value: "january" },
  { label: "2月", value: "february" },
  { label: "3月", value: "march" },
  { label: "4月", value: "april" },
  { label: "5月", value: "may" },
  { label: "6月", value: "june" },
  { label: "7月", value: "july" },
  { label: "8月", value: "august" },
  { label: "9月", value: "september" },
  { label: "10月", value: "october" },
  { label: "11月", value: "november" },
  { label: "12月", value: "december" },
]

export function ScheduleEditor({ value, error, onChange }: ScheduleEditorProps) {
  return (
    <FieldGroup>
      <Field>
        <FieldLabel htmlFor="scheduleKind">触发器类型</FieldLabel>
        <Select
          items={TRIGGER_TYPES}
          value={value.kind}
          onValueChange={(kind) => {
            if (kind) onChange(defaultSchedule(kind as ScheduleKind))
          }}
        >
          <SelectTrigger id="scheduleKind" className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              {TRIGGER_TYPES.map((item) => (
                <SelectItem key={item.value} value={item.value}>
                  {item.label}
                </SelectItem>
              ))}
            </SelectGroup>
          </SelectContent>
        </Select>
        <FieldDescription>配置会自动保存；点击“安装/更新任务”后才会应用到 Windows。</FieldDescription>
      </Field>

      {value.kind === "daily" ? (
        <div className="grid gap-4 sm:grid-cols-2">
          <TimeField
            id="dailyTime"
            label="执行时间"
            value={value.time}
            onChange={(time) => onChange({ ...value, time })}
          />
          <NumberField
            id="everyDays"
            label="每隔几天"
            value={value.everyDays}
            minimum={1}
            maximum={365}
            onChange={(everyDays) => onChange({ ...value, everyDays })}
          />
        </div>
      ) : null}

      {value.kind === "weekly" ? (
        <FieldGroup>
          <div className="grid gap-4 sm:grid-cols-2">
            <TimeField
              id="weeklyTime"
              label="执行时间"
              value={value.time}
              onChange={(time) => onChange({ ...value, time })}
            />
            <NumberField
              id="everyWeeks"
              label="每隔几周"
              value={value.everyWeeks}
              minimum={1}
              maximum={52}
              onChange={(everyWeeks) => onChange({ ...value, everyWeeks })}
            />
          </div>
          <FieldSet>
            <FieldLegend variant="label">星期</FieldLegend>
            <ToggleGroup
              multiple
              value={value.days}
              onValueChange={(days) => onChange({ ...value, days: days as Weekday[] })}
              variant="outline"
              spacing={2}
              className="flex-wrap"
            >
              {WEEKDAYS.map((day) => (
                <ToggleGroupItem key={day.value} value={day.value} aria-label={`周${day.label}`}>
                  {day.label}
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
          </FieldSet>
        </FieldGroup>
      ) : null}

      {value.kind === "monthly" ? (
        <FieldGroup>
          <div className="grid gap-4 sm:grid-cols-3">
            <TimeField
              id="monthlyTime"
              label="执行时间"
              value={value.time}
              onChange={(time) => onChange({ ...value, time })}
            />
            <Field>
              <FieldLabel htmlFor="monthlyDayKind">日期类型</FieldLabel>
              <Select
                items={[
                  { label: "指定日期", value: "day" },
                  { label: "最后一天", value: "lastDay" },
                ]}
                value={value.day.kind}
                onValueChange={(kind) => {
                  if (kind === "day") onChange({ ...value, day: { kind: "day", day: 1 } })
                  if (kind === "lastDay") onChange({ ...value, day: { kind: "lastDay" } })
                }}
              >
                <SelectTrigger id="monthlyDayKind" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="day">指定日期</SelectItem>
                    <SelectItem value="lastDay">最后一天</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            {value.day.kind === "day" ? (
              <NumberField
                id="monthlyDay"
                label="每月第几天"
                value={value.day.day}
                minimum={1}
                maximum={31}
                onChange={(day) => onChange({ ...value, day: { kind: "day", day } })}
              />
            ) : (
              <Field>
                <FieldLabel>执行日期</FieldLabel>
                <FieldDescription>所选月份的最后一天。</FieldDescription>
              </Field>
            )}
          </div>
          <FieldSet>
            <FieldLegend variant="label">月份（不选表示全年）</FieldLegend>
            <div data-slot="checkbox-group" className="grid gap-2 sm:grid-cols-4">
              {MONTHS.map((month) => {
                const checked = value.months.includes(month.value)
                return (
                  <Field key={month.value} orientation="horizontal">
                    <Checkbox
                      id={`month-${month.value}`}
                      checked={checked}
                      onCheckedChange={(nextChecked) => {
                        const months = nextChecked
                          ? [...value.months, month.value]
                          : value.months.filter((candidate) => candidate !== month.value)
                        onChange({ ...value, months })
                      }}
                    />
                    <FieldLabel htmlFor={`month-${month.value}`}>{month.label}</FieldLabel>
                  </Field>
                )
              })}
            </div>
          </FieldSet>
        </FieldGroup>
      ) : null}

      {value.kind === "interval" ? (
        <div className="grid gap-4 sm:grid-cols-3">
          <Field>
            <FieldLabel htmlFor="intervalUnit">间隔单位</FieldLabel>
            <Select
              items={[
                { label: "分钟", value: "minutes" },
                { label: "小时", value: "hours" },
              ]}
              value={value.unit}
              onValueChange={(unit) => {
                if (unit === "minutes" || unit === "hours") {
                  onChange({ ...value, unit, every: 1 })
                }
              }}
            >
              <SelectTrigger id="intervalUnit" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  <SelectItem value="minutes">分钟</SelectItem>
                  <SelectItem value="hours">小时</SelectItem>
                </SelectGroup>
              </SelectContent>
            </Select>
          </Field>
          <NumberField
            id="triggerInterval"
            label="间隔"
            value={value.every}
            minimum={1}
            maximum={value.unit === "minutes" ? 1_439 : 23}
            onChange={(every) => onChange({ ...value, every })}
          />
          <TimeField
            id="intervalStartTime"
            label="对齐起始时间"
            value={value.startTime}
            onChange={(startTime) => onChange({ ...value, startTime })}
          />
        </div>
      ) : null}

      {value.kind === "atLogon" || value.kind === "atStartup" ? (
        <NumberField
          id="triggerDelaySeconds"
          label="触发后延迟（秒）"
          value={value.delaySeconds}
          minimum={0}
          maximum={9_999 * 60 + 59}
          onChange={(delaySeconds) => onChange({ ...value, delaySeconds })}
        />
      ) : null}

      {value.kind === "cron" ? (
        <FieldGroup>
          <Field data-invalid={Boolean(error)}>
            <FieldLabel htmlFor="cronExpression">五字段 Cron</FieldLabel>
            <Input
              id="cronExpression"
              value={value.expression}
              onChange={(event) => onChange({ ...value, expression: event.currentTarget.value })}
              placeholder="30 9 * * MON-FRI"
              aria-invalid={Boolean(error)}
            />
            <FieldDescription>minute hour day-of-month month day-of-week，使用 Windows 本地时区。</FieldDescription>
          </Field>
          <Alert>
            <AlertTitle>严格映射模式</AlertTitle>
            <AlertDescription>
              支持分钟/小时步长、每日、每周和每月日期。无法无损映射为单个 Windows
              trigger 的表达式会被拒绝，不会近似执行。
            </AlertDescription>
          </Alert>
        </FieldGroup>
      ) : null}

      <Field data-invalid={Boolean(error)}>
        <FieldLabel>计划摘要</FieldLabel>
        <FieldDescription>{scheduleSummary(value)}</FieldDescription>
        <FieldError>{error}</FieldError>
      </Field>
    </FieldGroup>
  )
}

function TimeField({
  id,
  label,
  value,
  onChange,
}: {
  id: string
  label: string
  value: string
  onChange: (value: string) => void
}) {
  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input id={id} value={value} onChange={(event) => onChange(event.currentTarget.value)} />
    </Field>
  )
}

function NumberField({
  id,
  label,
  value,
  minimum,
  maximum,
  onChange,
}: {
  id: string
  label: string
  value: number
  minimum: number
  maximum: number
  onChange: (value: number) => void
}) {
  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input
        id={id}
        type="number"
        min={minimum}
        max={maximum}
        value={value}
        onChange={(event) => onChange(Number(event.currentTarget.value))}
      />
    </Field>
  )
}

function defaultSchedule(kind: ScheduleKind): ScheduleConfig {
  switch (kind) {
    case "daily":
      return { kind, time: "08:40", everyDays: 1 }
    case "weekly":
      return { kind, time: "08:40", everyWeeks: 1, days: ["monday"] }
    case "monthly":
      return { kind, time: "08:40", day: { kind: "day", day: 1 }, months: [] }
    case "interval":
      return { kind, unit: "minutes", every: 30, startTime: "00:00" }
    case "atLogon":
    case "atStartup":
      return { kind, delaySeconds: 0 }
    case "cron":
      return { kind, expression: "30 9 * * MON-FRI" }
  }
}

function scheduleSummary(schedule: ScheduleConfig): string {
  switch (schedule.kind) {
    case "daily":
      return schedule.everyDays === 1
        ? `每天 ${schedule.time}`
        : `每 ${schedule.everyDays} 天 ${schedule.time}`
    case "weekly":
      return `每 ${schedule.everyWeeks} 周，${schedule.days.length} 个星期日，${schedule.time}`
    case "monthly":
      return `${schedule.months.length === 0 ? "每月" : `${schedule.months.length} 个月份`}的${schedule.day.kind === "lastDay" ? "最后一天" : `${schedule.day.day} 日`} ${schedule.time}`
    case "interval":
      return `从 ${schedule.startTime} 起每 ${schedule.every} ${schedule.unit === "minutes" ? "分钟" : "小时"}`
    case "atLogon":
      return `用户登录后延迟 ${schedule.delaySeconds} 秒`
    case "atStartup":
      return `Windows 启动后延迟 ${schedule.delaySeconds} 秒`
    case "cron":
      return `Cron：${schedule.expression || "尚未输入"}`
  }
}
