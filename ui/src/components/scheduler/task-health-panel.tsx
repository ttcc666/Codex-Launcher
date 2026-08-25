import { AlertTriangleIcon, RefreshCwIcon } from "lucide-react"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Spinner } from "@/components/ui/spinner"
import type { TaskHealth } from "@/hooks/useTauri"

interface TaskHealthPanelProps {
  health: TaskHealth | null
  loading: boolean
  onRefresh: () => void
}

export function TaskHealthPanel({ health, loading, onRefresh }: TaskHealthPanelProps) {
  const overall = overallHealth(health, loading)

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          Windows 计划任务健康状态
          <Badge variant={overall.variant}>{overall.label}</Badge>
        </CardTitle>
        <CardDescription>结构化读取 Task Scheduler 的注册、运行结果与配置漂移。</CardDescription>
        <CardAction>
          <Button variant="outline" size="sm" onClick={onRefresh} disabled={loading}>
            {loading ? (
              <Spinner data-icon="inline-start" />
            ) : (
              <RefreshCwIcon data-icon="inline-start" />
            )}
            刷新
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {!health ? (
          <p className="text-sm text-muted-foreground">
            {loading ? "正在读取 Windows Task Scheduler…" : "暂无可展示的健康数据。"}
          </p>
        ) : (
          <>
            <dl className="grid gap-3 text-sm sm:grid-cols-2 lg:grid-cols-3">
              <HealthMetric label="安装状态" value={health.installed ? "已注册" : "未注册"} />
              <HealthMetric label="Windows State" value={health.stateLabel ?? "N/A"} />
              <HealthMetric label="Enabled" value={health.enabled ? "是" : "否"} />
              <HealthMetric label="下次运行" value={formatTaskTime(health.nextRunTime)} />
              <HealthMetric label="上次运行" value={formatTaskTime(health.lastRunTime)} />
              <HealthMetric
                label="上次结果"
                value={
                  health.lastResultLabel
                    ? `${health.lastResultLabel}${health.lastResultHex ? ` (${health.lastResultHex})` : ""}`
                    : "N/A"
                }
              />
              <HealthMetric label="错过运行" value={String(health.missedRuns)} />
              <HealthMetric label="应用管理" value={health.managed ? "Managed" : "未登记"} />
              <HealthMetric
                label="配置同步"
                value={health.configDrift ? "待应用" : "已同步"}
              />
            </dl>

            <div className="grid gap-3 text-sm sm:grid-cols-2">
              <HealthMetric label="期望计划" value={health.desiredScheduleSummary} />
              <HealthMetric
                label="已应用计划"
                value={health.appliedScheduleSummary ?? "没有 managed 记录"}
              />
            </div>

            <div className="flex flex-col gap-2 rounded-lg border bg-muted/30 p-3 text-sm">
              <div>
                <span className="font-medium">Action：</span>
                <code className="break-all text-muted-foreground">
                  {health.actionPath ?? "N/A"} {health.actionArguments ?? ""}
                </code>
              </div>
              <div className="flex flex-wrap gap-2">
                <Badge variant={health.actionMatchesApp ? "secondary" : "destructive"}>
                  {health.actionMatchesApp ? "Action 匹配当前应用" : "Action 不匹配"}
                </Badge>
                {health.staleManagedTasks.map((taskName) => (
                  <Badge key={taskName} variant="outline">
                    旧任务：{taskName}
                  </Badge>
                ))}
              </div>
            </div>

            {health.warnings.length > 0 ? (
              <Alert variant="destructive">
                <AlertTriangleIcon />
                <AlertTitle>需要处理</AlertTitle>
                <AlertDescription>
                  <ul className="list-inside list-disc">
                    {health.warnings.map((warning) => (
                      <li key={warning}>{warning}</li>
                    ))}
                  </ul>
                </AlertDescription>
              </Alert>
            ) : (
              <Alert>
                <AlertTitle>计划任务健康</AlertTitle>
                <AlertDescription>注册、Action 和已应用计划均与当前配置一致。</AlertDescription>
              </Alert>
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

function HealthMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border bg-card p-3">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 break-words font-medium">{value}</dd>
    </div>
  )
}

function formatTaskTime(value: string | null): string {
  if (!value) return "N/A"
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString("zh-CN", { hour12: false })
}

function overallHealth(health: TaskHealth | null, loading: boolean): {
  label: string
  variant: "default" | "secondary" | "destructive" | "outline"
} {
  if (loading && !health) return { label: "加载中", variant: "secondary" }
  if (!health?.installed) return { label: "Missing", variant: "outline" }
  if (health.state === "running") return { label: "Running", variant: "default" }
  if (health.warnings.length > 0) return { label: "Warning", variant: "destructive" }
  return { label: "Healthy", variant: "secondary" }
}
