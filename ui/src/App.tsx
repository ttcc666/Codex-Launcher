import { useEffect, useMemo, useRef, useState } from "react"
import { invoke } from "@tauri-apps/api/core"
import { useVirtualizer } from "@tanstack/react-virtual"
import {
  ActivityIcon,
  AlertTriangleIcon,
  BellIcon,
  CpuIcon,
  ExternalLinkIcon,
  FileTerminalIcon,
  FolderOpenIcon,
  KeyRoundIcon,
  MailIcon,
  PlayIcon,
  SearchIcon,
  ServerCrashIcon,
  SendIcon,
  SquareIcon,
  TimerIcon,
  Trash2Icon,
  XIcon,
  ZapIcon,
} from "lucide-react"
import { toast } from "sonner"

import { ModeToggle } from "@/components/mode-toggle"
import { AnimatedContent } from "@/components/ui/AnimatedContent"
import { BorderGlow } from "@/components/ui/BorderGlow"
import { ClickSpark } from "@/components/ui/ClickSpark"
import { CountUp } from "@/components/ui/CountUp"
import { GradualBlur } from "@/components/ui/GradualBlur"
import { ShinyText } from "@/components/ui/ShinyText"
import { SpotlightCard } from "@/components/ui/SpotlightCard"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogMedia,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Field, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput,
} from "@/components/ui/input-group"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { validateConfig } from "@/hooks/configValidation"
import {
  useTauri,
  type AppConfig,
  type EmailCredentialStatus,
  type ServerChanCredentialStatus,
} from "@/hooks/useTauri"
import { cn } from "@/lib/utils"

type LogLevel = "error" | "warn" | "success" | "info" | "rate_limit" | "default"

function parseLogLine(rawLine: string) {
  let timestamp = ""
  let content = rawLine

  const timestampMatch = rawLine.match(
    /^(\[\d{4}-\d{2}-\d{2}\s\d{2}:\d{2}:\d{2}(?:\.\d+)?\]|\[\d{2}:\d{2}:\d{2}(?:\.\d+)?\]|\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?)\s*/,
  )
  if (timestampMatch) {
    timestamp = timestampMatch[1]
    content = rawLine.slice(timestampMatch[0].length)
  }

  let level: LogLevel = "default"
  const upperContent = rawLine.toUpperCase()

  if (
    upperContent.includes("ERROR") ||
    upperContent.includes("ERR:") ||
    upperContent.includes("FAIL") ||
    rawLine.includes("失败") ||
    rawLine.includes("异常")
  ) {
    level = "error"
  } else if (
    upperContent.includes("429") ||
    upperContent.includes("HIGH_DEMAND") ||
    rawLine.includes("拥堵") ||
    rawLine.includes("拦截")
  ) {
    level = "rate_limit"
  } else if (
    upperContent.includes("WARN") ||
    upperContent.includes("RETRY") ||
    rawLine.includes("警告") ||
    rawLine.includes("重试")
  ) {
    level = "warn"
  } else if (
    upperContent.includes("SUCCESS") ||
    upperContent.includes("OK") ||
    rawLine.includes("成功") ||
    rawLine.includes("正常完成")
  ) {
    level = "success"
  } else if (
    upperContent.includes("INFO") ||
    rawLine.includes("信息") ||
    rawLine.includes("开始") ||
    rawLine.includes("启动")
  ) {
    level = "info"
  }

  return { timestamp, content, level }
}

function escapeRegExp(string: string) {
  return string.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

function highlightUrlsAndPaths(text: string, level: LogLevel) {
  const urlRegex = /(https?:\/\/[^\s]+)/g
  const parts = text.split(urlRegex)
  return parts.map((part, idx) => {
    if (part.match(urlRegex)) {
      return (
        <a
          key={idx}
          href={part}
          target="_blank"
          rel="noreferrer"
          className="text-sky-400 underline underline-offset-2 hover:text-sky-300 transition-colors"
        >
          {part}
        </a>
      )
    }
    return (
      <span
        key={idx}
        className={cn(
          level === "error" && "text-rose-300 font-medium",
          level === "warn" && "text-amber-200",
          level === "success" && "text-emerald-200 font-medium",
          level === "rate_limit" && "text-purple-200 font-medium",
          level === "info" && "text-zinc-100",
          level === "default" && "text-zinc-200",
        )}
      >
        {part}
      </span>
    )
  })
}

function FormattedLogLine({
  line,
  index,
  searchTerm,
}: {
  line: string
  index: number
  searchTerm?: string
}) {
  const { timestamp, content, level } = useMemo(() => parseLogLine(line), [line])

  const badgeMap = {
    error: (
      <span className="shrink-0 select-none rounded bg-rose-500/15 px-1.5 py-0.5 text-[10px] font-bold tracking-wider text-rose-400 border border-rose-500/30">
        ERROR
      </span>
    ),
    rate_limit: (
      <span className="shrink-0 select-none rounded bg-purple-500/15 px-1.5 py-0.5 text-[10px] font-bold tracking-wider text-purple-300 border border-purple-500/30">
        429 LIMIT
      </span>
    ),
    warn: (
      <span className="shrink-0 select-none rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-bold tracking-wider text-amber-300 border border-amber-500/30">
        WARN
      </span>
    ),
    success: (
      <span className="shrink-0 select-none rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] font-bold tracking-wider text-emerald-300 border border-emerald-500/30">
        SUCCESS
      </span>
    ),
    info: (
      <span className="shrink-0 select-none rounded bg-sky-500/15 px-1.5 py-0.5 text-[10px] font-bold tracking-wider text-sky-300 border border-sky-500/30">
        INFO
      </span>
    ),
    default: null,
  }

  const renderContent = () => {
    if (!searchTerm) {
      return highlightUrlsAndPaths(content, level)
    }
    const parts = content.split(new RegExp(`(${escapeRegExp(searchTerm)})`, "gi"))
    return parts.map((part, i) =>
      part.toLowerCase() === searchTerm.toLowerCase() ? (
        <mark key={i} className="rounded bg-amber-400/30 px-0.5 text-amber-200">
          {part}
        </mark>
      ) : (
        highlightUrlsAndPaths(part, level)
      ),
    )
  }

  return (
    <div
      className={cn(
        "group flex items-start gap-2.5 px-3 py-1 font-mono text-[12.5px] leading-relaxed transition-colors border-b border-zinc-900/30",
        "hover:bg-zinc-900/70",
        level === "error" && "bg-rose-950/20 hover:bg-rose-950/30",
        level === "rate_limit" && "bg-purple-950/20 hover:bg-purple-950/30",
        level === "success" && "bg-emerald-950/10 hover:bg-emerald-950/20",
      )}
    >
      <span className="w-9 shrink-0 select-none font-mono text-[11px] text-zinc-600 group-hover:text-zinc-400 text-right pr-1">
        {index + 1}
      </span>

      {timestamp ? (
        <span className="shrink-0 select-none font-mono text-[11px] text-zinc-500 group-hover:text-zinc-400">
          {timestamp}
        </span>
      ) : null}

      {badgeMap[level]}

      <div className="min-w-0 flex-1 break-all text-zinc-200">
        {renderContent()}
      </div>
    </div>
  )
}

export default function App() {
  const {
    config,
    setConfig,
    state,
    taskStatus,
    logs,
    beginRun,
    clearVisibleLogs,
    isTaskInstalled,
    taskDetail,
    checkTask,
    errors,
    dismissError,
  } = useTauri()
  const [isStarting, setIsStarting] = useState(false)
  const [isStopping, setIsStopping] = useState(false)
  const [isUpdatingKeepAlive, setIsUpdatingKeepAlive] = useState(false)
  const [currentKeepAliveOverride, setCurrentKeepAliveOverride] = useState<boolean>()
  const [isManualKeepAliveStarting, setIsManualKeepAliveStarting] = useState(false)
  const [isManualKeepAliveStopping, setIsManualKeepAliveStopping] = useState(false)
  const [manualKeepAliveRunId, setManualKeepAliveRunId] = useState<string>()
  const [autoScroll, setAutoScroll] = useState(true)
  const [historyDialogOpen, setHistoryDialogOpen] = useState(false)
  const [logFilter, setLogFilter] = useState("")
  const [onlyErrors, setOnlyErrors] = useState(false)
  const [serverChanStatus, setServerChanStatus] = useState<ServerChanCredentialStatus>({
    configured: false,
  })
  const [serverChanSendKey, setServerChanSendKey] = useState("")
  const [serverChanAction, setServerChanAction] = useState<
    "loading" | "saving" | "testing" | "deleting"
  >()
  const [serverChanLastResult, setServerChanLastResult] = useState("")
  const [isTestingDesktopNotification, setIsTestingDesktopNotification] = useState(false)
  const [desktopNotificationLastResult, setDesktopNotificationLastResult] = useState("")
  const [emailStatus, setEmailStatus] = useState<EmailCredentialStatus>({ configured: false })
  const [emailSmtpPassword, setEmailSmtpPassword] = useState("")
  const [emailAction, setEmailAction] = useState<"loading" | "saving" | "testing" | "deleting">()
  const [emailLastResult, setEmailLastResult] = useState("")
  const scrollViewportRef = useRef<HTMLDivElement>(null)

  const validationErrors = useMemo(() => validateConfig(config), [config])
  const configIsValid = Object.keys(validationErrors).length === 0
  const isManualKeepAliveActive =
    state.isRunning &&
    (taskStatus?.runMode === "manualKeepAlive" || state.runId === manualKeepAliveRunId)
  const isRetryRunActive = state.isRunning && taskStatus?.runMode === "retry"
  const currentRetryKeepAliveEnabled =
    isRetryRunActive &&
    (currentKeepAliveOverride ?? taskStatus?.keepAliveEnabled ?? false)
  const canControlRetryKeepAlive =
    isRetryRunActive && !isUpdatingKeepAlive && !isStopping
  const manualKeepAliveDisabled =
    isManualKeepAliveStarting ||
    isManualKeepAliveStopping ||
    isStarting ||
    isStopping ||
    (!isManualKeepAliveActive && (state.isRunning || !configIsValid))

  const filteredLogs = useMemo(() => {
    let result = logs
    if (onlyErrors) {
      result = result.filter((l) => {
        const u = l.toUpperCase()
        return (
          u.includes("ERROR") ||
          u.includes("FAIL") ||
          l.includes("失败") ||
          l.includes("异常") ||
          u.includes("429")
        )
      })
    }
    if (logFilter.trim()) {
      const keyword = logFilter.toLowerCase()
      result = result.filter((l) => l.toLowerCase().includes(keyword))
    }
    return result
  }, [logs, logFilter, onlyErrors])

  const logVirtualizer = useVirtualizer({
    count: filteredLogs.length,
    getScrollElement: () => scrollViewportRef.current,
    estimateSize: () => 28,
    overscan: 20,
  })

  useEffect(() => {
    const error = errors[0]
    if (!error) return
    toast.error(error.action, {
      description: `[${error.source}] ${error.message}`,
    })
    dismissError(error.id)
  }, [dismissError, errors])

  useEffect(() => {
    if (!state.isRunning) {
      setIsStopping(false)
      setIsManualKeepAliveStopping(false)
      setManualKeepAliveRunId(undefined)
    }
  }, [state.isRunning])

  useEffect(() => {
    setCurrentKeepAliveOverride(undefined)
  }, [state.runId])

  useEffect(() => {
    let disposed = false
    setServerChanAction("loading")
    invoke<ServerChanCredentialStatus>("get_server_chan_status")
      .then((status) => {
        if (!disposed) setServerChanStatus(status)
      })
      .catch((error: unknown) => {
        if (!disposed) showActionError("读取 Server酱凭据状态失败", error)
      })
      .finally(() => {
        if (!disposed) setServerChanAction(undefined)
      })
    return () => {
      disposed = true
    }
  }, [])

  useEffect(() => {
    let disposed = false
    setEmailAction("loading")
    invoke<EmailCredentialStatus>("get_email_status")
      .then((status) => {
        if (!disposed) setEmailStatus(status)
      })
      .catch((error: unknown) => {
        if (!disposed) showActionError("读取邮件凭据状态失败", error)
      })
      .finally(() => {
        if (!disposed) setEmailAction(undefined)
      })
    return () => {
      disposed = true
    }
  }, [])

  useEffect(() => {
    if (!autoScroll || filteredLogs.length === 0) return

    logVirtualizer.scrollToIndex(filteredLogs.length - 1, { align: "end" })

    const timer1 = setTimeout(() => {
      if (scrollViewportRef.current) {
        scrollViewportRef.current.scrollTop = scrollViewportRef.current.scrollHeight
      }
    }, 10)

    const timer2 = setTimeout(() => {
      if (scrollViewportRef.current) {
        scrollViewportRef.current.scrollTop = scrollViewportRef.current.scrollHeight
      }
    }, 60)

    return () => {
      clearTimeout(timer1)
      clearTimeout(timer2)
    }
  }, [autoScroll, filteredLogs.length, logVirtualizer])

  const handleConfigChange = <Key extends keyof AppConfig>(
    key: Key,
    value: AppConfig[Key],
  ) => setConfig((current) => ({ ...current, [key]: value }))

  const setServerChanEnabled = (enabled: boolean) =>
    setConfig((current) => ({
      ...current,
      serverChan: { enabled },
    }))

  const setDesktopNotificationEnabled = (enabled: boolean) =>
    setConfig((current) => ({
      ...current,
      desktopNotification: { enabled },
    }))

  const testDesktopNotification = async () => {
    setIsTestingDesktopNotification(true)
    try {
      const message = await invoke<string>("test_desktop_notification")
      setDesktopNotificationLastResult(message)
      toast.success("桌面测试通知已发送", { description: message })
    } catch (error: unknown) {
      setDesktopNotificationLastResult(actionErrorMessage(error))
      showActionError("发送桌面测试通知失败", error)
    } finally {
      setIsTestingDesktopNotification(false)
    }
  }

  const saveServerChanSendKey = async () => {
    if (!serverChanSendKey.trim()) {
      toast.error("SendKey 不能为空")
      return
    }
    setServerChanAction("saving")
    try {
      const status = await invoke<ServerChanCredentialStatus>("set_server_chan_send_key", {
        sendKey: serverChanSendKey,
      })
      setServerChanStatus(status)
      setServerChanSendKey("")
      setServerChanLastResult("凭据已保存到 Windows Credential Manager")
      toast.success("Server酱 SendKey 已安全保存")
    } catch (error: unknown) {
      showActionError("保存 Server酱 SendKey 失败", error)
    } finally {
      setServerChanAction(undefined)
    }
  }

  const testServerChanNotification = async () => {
    setServerChanAction("testing")
    try {
      const message = await invoke<string>("test_server_chan_notification")
      setServerChanLastResult(message)
      toast.success("个人微信测试通知已发送", { description: message })
    } catch (error: unknown) {
      const message = actionErrorMessage(error)
      if (message.includes("通知可能已送达")) {
        setServerChanLastResult(message)
        toast.warning("测试通知发送结果未确认", { description: message })
      } else {
        setServerChanLastResult("")
        showActionError("发送测试通知失败", error)
      }
    } finally {
      setServerChanAction(undefined)
    }
  }

  const deleteServerChanSendKey = async () => {
    setServerChanAction("deleting")
    try {
      const status = await invoke<ServerChanCredentialStatus>("delete_server_chan_send_key")
      setServerChanStatus(status)
      setServerChanSendKey("")
      setServerChanLastResult("Server酱凭据已删除")
      toast.success("Server酱凭据已删除")
    } catch (error: unknown) {
      showActionError("删除 Server酱凭据失败", error)
    } finally {
      setServerChanAction(undefined)
    }
  }

  const setEmailEnabled = (enabled: boolean) =>
    setConfig((current) => ({
      ...current,
      email: { ...current.email, enabled },
    }))

  const saveEmailSmtpPassword = async () => {
    if (!emailSmtpPassword.trim()) {
      toast.error("SMTP 密码不能为空")
      return
    }
    setEmailAction("saving")
    try {
      const status = await invoke<EmailCredentialStatus>("set_email_smtp_password", {
        password: emailSmtpPassword,
      })
      setEmailStatus(status)
      setEmailSmtpPassword("")
      setEmailLastResult("SMTP 密码已保存到 Windows Credential Manager")
      toast.success("SMTP 密码已安全保存")
    } catch (error: unknown) {
      showActionError("保存 SMTP 密码失败", error)
    } finally {
      setEmailAction(undefined)
    }
  }

  const testEmailNotification = async () => {
    setEmailAction("testing")
    try {
      const message = await invoke<string>("test_email_notification", { config })
      setEmailLastResult(message)
      toast.success("测试邮件已发送", { description: message })
    } catch (error: unknown) {
      setEmailLastResult("")
      showActionError("发送测试邮件失败", error)
    } finally {
      setEmailAction(undefined)
    }
  }

  const deleteEmailSmtpPassword = async () => {
    setEmailAction("deleting")
    try {
      const status = await invoke<EmailCredentialStatus>("delete_email_smtp_password")
      setEmailStatus(status)
      setEmailSmtpPassword("")
      setEmailLastResult("SMTP 密码已删除")
      toast.success("邮件 SMTP 密码已删除")
    } catch (error: unknown) {
      showActionError("删除 SMTP 密码失败", error)
    } finally {
      setEmailAction(undefined)
    }
  }

  const setCurrentRetryKeepAlive = async (checked: boolean) => {
    if (!isRetryRunActive || !state.runId) return

    setIsUpdatingKeepAlive(true)
    try {
      const message = await invoke<string>("set_run_keep_alive", {
        runId: state.runId,
        enabled: checked,
      })
      setCurrentKeepAliveOverride(checked)
      toast.info(checked ? "本次 run 已开启保活" : "本次 run 已关闭保活", {
        description: message,
      })
    } catch (error: unknown) {
      showActionError("切换运行时保活失败", error)
    } finally {
      setIsUpdatingKeepAlive(false)
    }
  }

  const browseWorkDir = async () => {
    try {
      const path = await invoke<string | null>("select_work_directory")
      if (path) handleConfigChange("workDir", path)
    } catch (error: unknown) {
      showActionError("选择工作目录失败", error)
    }
  }

  const startRetry = async () => {
    if (!configIsValid) {
      toast.error("配置无效", { description: firstValidationError(validationErrors) })
      return
    }
    setIsStarting(true)
    try {
      const runId = await invoke<string>("start_retry", { config })
      beginRun(runId)
      toast.success("重试引擎已启动")
    } catch (error: unknown) {
      showActionError("启动失败", error)
    } finally {
      setIsStarting(false)
    }
  }

  const stopRetry = async () => {
    if (!state.runId) {
      toast.error("当前没有可停止的 run")
      return
    }
    setIsStopping(true)
    try {
      const message = await invoke<string>("stop_retry", { runId: state.runId })
      toast.info("停止请求已发送", { description: message })
    } catch (error: unknown) {
      setIsStopping(false)
      showActionError("停止失败", error)
    }
  }

  const startManualKeepAlive = async () => {
    if (!configIsValid) {
      toast.error("配置无效", { description: firstValidationError(validationErrors) })
      return
    }

    setIsManualKeepAliveStarting(true)
    try {
      const runId = await invoke<string>("start_manual_keep_alive", { config })
      setManualKeepAliveRunId(runId)
      beginRun(runId)
      toast.success("手动保活已启动")
    } catch (error: unknown) {
      showActionError("启动手动保活失败", error)
    } finally {
      setIsManualKeepAliveStarting(false)
    }
  }

  const stopManualKeepAlive = async () => {
    if (!state.runId) {
      toast.error("当前没有可停止的手动保活 run")
      return
    }

    setIsManualKeepAliveStopping(true)
    try {
      const message = await invoke<string>("stop_retry", { runId: state.runId })
      toast.info("手动保活停止请求已发送", { description: message })
    } catch (error: unknown) {
      setIsManualKeepAliveStopping(false)
      showActionError("停止手动保活失败", error)
    }
  }

  const toggleManualKeepAlive = async (checked: boolean) => {
    if (checked) {
      await startManualKeepAlive()
    } else {
      await stopManualKeepAlive()
    }
  }

  const openDashboard = async () => {
    try {
      await invoke("open_dashboard_url")
    } catch (error: unknown) {
      showActionError("打开仪表盘失败", error)
    }
  }

  const openLogDir = async () => {
    try {
      await invoke("open_log_directory")
    } catch (error: unknown) {
      showActionError("打开日志目录失败", error)
    }
  }

  const clearHistoryLogs = async () => {
    setHistoryDialogOpen(false)
    try {
      const message = await invoke<string>("clear_history_logs_command")
      toast.success(message)
    } catch (error: unknown) {
      showActionError("清理历史日志失败", error)
    }
  }

  const installTask = async () => {
    if (!configIsValid) {
      toast.error("配置无效", { description: firstValidationError(validationErrors) })
      return
    }
    try {
      const message = await invoke<string>("install_task", { config })
      toast.success(message)
      await checkTask()
    } catch (error: unknown) {
      showActionError("安装计划任务失败", error)
    }
  }

  const uninstallTask = async () => {
    if (validationErrors.taskName) {
      toast.error("任务名称无效", { description: validationErrors.taskName })
      return
    }
    try {
      const message = await invoke<string>("uninstall_task", {
        taskName: config.taskName,
      })
      toast.success(message)
      await checkTask()
    } catch (error: unknown) {
      showActionError("移除计划任务失败", error)
    }
  }

  return (
    <div className="relative min-h-screen bg-background font-sans text-foreground antialiased overflow-x-hidden">
      {/* React Bits Ambient Aurora & Cyber Grid Background */}
      <div className="pointer-events-none fixed inset-0 z-0 overflow-hidden select-none">
        {/* Cyber Grid Pattern */}
        <div className="absolute inset-0 bg-grid-pattern opacity-20 dark:opacity-40" />
        
        {/* Ambient Aurora Blobs */}
        <div className="aurora-blob-1 absolute -top-[15%] -left-[10%] h-[550px] w-[550px] rounded-full bg-emerald-500/5 dark:bg-emerald-500/10 blur-[130px]" />
        <div className="aurora-blob-2 absolute top-[35%] -right-[10%] h-[650px] w-[650px] rounded-full bg-purple-500/5 dark:bg-purple-500/10 blur-[150px]" />
        <div className="aurora-blob-1 absolute -bottom-[15%] left-[20%] h-[450px] w-[450px] rounded-full bg-sky-500/5 dark:bg-sky-500/10 blur-[130px]" />
      </div>

      <div className="relative z-10">
        <header className="border-b bg-background/80 backdrop-blur sticky top-0 z-50">
          <div className="container mx-auto flex h-14 items-center px-4 md:px-6">
            <div className="flex items-center gap-2.5 font-bold tracking-tight">
              <div className="rounded-lg bg-emerald-500/10 p-1.5 text-emerald-600 dark:text-emerald-400 border border-emerald-500/20 shadow-[0_0_10px_rgba(16,185,129,0.15)]">
                <CpuIcon className="size-5" />
              </div>
              <ShinyText text="Codex Launcher" speed={6} className="text-base font-semibold" />
              <span className="text-[11px] font-mono font-normal text-muted-foreground bg-muted px-2 py-0.5 rounded-full border">
                v2.0
              </span>
            </div>

            <div className="ml-auto flex items-center gap-3">
              {statusBadge(state.status)}
              <div className="h-4 w-[1px] bg-border" />
              <ModeToggle />
            </div>
          </div>
        </header>

        <main className="container mx-auto flex flex-col gap-8 p-4 md:p-6 lg:p-8">
          <AnimatedContent
            className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between"
            distance={12}
          >
            <div>
              <h1 className="text-3xl font-bold tracking-tight">
                <ShinyText text="Dashboard" speed={5} />
              </h1>
              <p className="mt-1 text-sm text-muted-foreground">Codex 重试引擎控制中心</p>
            </div>
          <div className="flex flex-wrap items-center gap-2 sm:justify-end">
            {!state.isRunning ? (
              <ClickSpark
                color="#34d399"
                disabled={isStarting || isManualKeepAliveStarting || !configIsValid}
              >
                <Button
                  onClick={startRetry}
                  disabled={isStarting || isManualKeepAliveStarting || !configIsValid}
                  className="bg-emerald-600 hover:bg-emerald-500 text-white font-medium shadow-[0_0_15px_rgba(16,185,129,0.35)] hover:shadow-[0_0_22px_rgba(16,185,129,0.55)] active:scale-95 transition-all duration-200"
                >
                  {isStarting ? <Spinner data-icon="inline-start" /> : <PlayIcon data-icon="inline-start" />}
                  {isStarting ? "启动中..." : "启动重试引擎"}
                </Button>
              </ClickSpark>
            ) : (
              <ClickSpark
                color="#fb7185"
                disabled={isStopping || isManualKeepAliveStopping}
              >
                <Button
                  onClick={stopRetry}
                  disabled={isStopping || isManualKeepAliveStopping}
                  variant="destructive"
                  className="shadow-[0_0_15px_rgba(244,63,94,0.35)] hover:shadow-[0_0_22px_rgba(244,63,94,0.55)] active:scale-95 transition-all duration-200"
                >
                  {isStopping ? <Spinner data-icon="inline-start" /> : <SquareIcon data-icon="inline-start" />}
                  {isStopping ? "停止中..." : "停止引擎"}
                </Button>
              </ClickSpark>
            )}
            <div
              className={cn(
                "flex h-9 w-[112px] items-center justify-center gap-2 rounded-md border px-2.5",
                isRetryRunActive
                  ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
                  : "border-border bg-muted/40 text-muted-foreground",
              )}
              title={
                isRetryRunActive
                  ? "独立控制当前 retry run 的保活，不修改配置页偏好"
                  : "启动重试引擎后可控制当前 run 的保活"
              }
            >
              <Switch
                id="currentRetryKeepAlive"
                checked={currentRetryKeepAliveEnabled}
                disabled={!canControlRetryKeepAlive}
                onCheckedChange={(checked) => void setCurrentRetryKeepAlive(checked)}
                className="scale-75 data-checked:bg-emerald-500"
              />
              <label
                htmlFor="currentRetryKeepAlive"
                className={cn(
                  "whitespace-nowrap text-xs font-medium select-none",
                  canControlRetryKeepAlive ? "cursor-pointer" : "cursor-not-allowed",
                )}
              >
                本次保活
              </label>
            </div>
            <div
              className={cn(
                "flex h-9 w-[112px] items-center justify-center gap-2 rounded-md border px-2.5",
                isManualKeepAliveActive
                  ? "border-sky-500/40 bg-sky-500/10 text-sky-700 dark:text-sky-300"
                  : "border-border bg-muted/40 text-muted-foreground",
              )}
              title="使用当前配置直接启动或停止独立保活循环"
            >
              <Switch
                id="manualKeepAlive"
                checked={isManualKeepAliveActive || isManualKeepAliveStarting}
                disabled={manualKeepAliveDisabled}
                onCheckedChange={(checked) => void toggleManualKeepAlive(checked)}
                className="scale-75 data-checked:bg-sky-500"
              />
              <label
                htmlFor="manualKeepAlive"
                className={cn(
                  "whitespace-nowrap text-xs font-medium select-none",
                  manualKeepAliveDisabled ? "cursor-not-allowed" : "cursor-pointer",
                )}
              >
                手动保活
              </label>
            </div>
            <Button
              variant="outline"
              size="icon"
              onClick={openDashboard}
              title="打开 Web 仪表盘"
            >
              <ExternalLinkIcon data-icon="inline-start" />
              <span className="sr-only">打开 Web 仪表盘</span>
            </Button>
          </div>
        </AnimatedContent>

        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
          <AnimatedContent className="h-full" delay={60}>
            <MetricCard title="系统状态" icon={<ActivityIcon className="size-4" />} tone="emerald">
              <div className="flex flex-col gap-1.5">
                {statusBadge(state.status)}
                {state.concurrency > 1 && (
                  <span className="text-xs font-medium text-muted-foreground">
                    并发 {state.activeWorkers}/{state.concurrency} 线程
                  </span>
                )}
              </div>
            </MetricCard>
          </AnimatedContent>
          <AnimatedContent className="h-full" delay={110}>
            <MetricCard title="重试次数" icon={<ZapIcon className="size-4" />} tone="sky">
              <CountUp value={state.attempt} />
            </MetricCard>
          </AnimatedContent>
          <AnimatedContent className="h-full" delay={160}>
            <MetricCard title="拦截拥堵请求" icon={<ServerCrashIcon className="size-4" />} tone="rose">
              <CountUp value={state.highDemandCount} />
            </MetricCard>
          </AnimatedContent>
          <AnimatedContent className="h-full" delay={210}>
            <MetricCard title="运行时长" icon={<TimerIcon className="size-4" />} mono tone="violet">
              {state.elapsedText}
            </MetricCard>
          </AnimatedContent>
        </div>

        <AnimatedContent className="w-full" delay={260} distance={12}>
          <Tabs defaultValue="logs" className="w-full">
          <TabsList className="grid w-full max-w-[520px] grid-cols-4">
            <TabsTrigger value="config">配置参数</TabsTrigger>
            <TabsTrigger value="logs">实时终端</TabsTrigger>
            <TabsTrigger value="task">计划任务</TabsTrigger>
            <TabsTrigger value="notifications">通知</TabsTrigger>
          </TabsList>

          <TabsContent value="config" className="mt-4">
            <Card>
              <CardHeader>
                <CardTitle>核心配置</CardTitle>
                <CardDescription>定义重试引擎的行为模式和触发条件。</CardDescription>
              </CardHeader>
              <CardContent>
                <FieldGroup>
                  <Field data-invalid={Boolean(validationErrors.command)}>
                    <FieldLabel htmlFor="command">执行命令</FieldLabel>
                    <Input
                      id="command"
                      value={config.command}
                      onChange={(event) => handleConfigChange("command", event.currentTarget.value)}
                      placeholder="如: npm run dev"
                      aria-invalid={Boolean(validationErrors.command)}
                    />
                    <FieldError>{validationErrors.command}</FieldError>
                  </Field>

                  <Field data-invalid={Boolean(validationErrors.workDir)}>
                    <FieldLabel htmlFor="workDir">工作目录</FieldLabel>
                    <InputGroup>
                      <InputGroupInput
                        id="workDir"
                        value={config.workDir}
                        onChange={(event) => handleConfigChange("workDir", event.currentTarget.value)}
                        placeholder="请选择 command 的工作目录"
                        aria-invalid={Boolean(validationErrors.workDir)}
                      />
                      <InputGroupAddon align="inline-end">
                        <InputGroupButton onClick={browseWorkDir}>
                          <FolderOpenIcon data-icon="inline-start" />
                          浏览
                        </InputGroupButton>
                      </InputGroupAddon>
                    </InputGroup>
                    <FieldError>{validationErrors.workDir}</FieldError>
                  </Field>

                  <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                    <Field data-invalid={Boolean(validationErrors.interval)}>
                      <FieldLabel htmlFor="interval">重试间隔（秒）</FieldLabel>
                      <Input
                        id="interval"
                        type="number"
                        min={1}
                        max={86_400}
                        value={config.interval}
                        onChange={(event) => handleConfigChange("interval", Number(event.currentTarget.value))}
                        aria-invalid={Boolean(validationErrors.interval)}
                      />
                      <FieldError>{validationErrors.interval}</FieldError>
                    </Field>
                    <Field data-invalid={Boolean(validationErrors.maxTries)}>
                      <FieldLabel htmlFor="maxTries">最大尝试次数（0 为无限）</FieldLabel>
                      <Input
                        id="maxTries"
                        type="number"
                        min={0}
                        max={100_000}
                        value={config.maxTries}
                        onChange={(event) => handleConfigChange("maxTries", Number(event.currentTarget.value))}
                        aria-invalid={Boolean(validationErrors.maxTries)}
                      />
                      <FieldError>{validationErrors.maxTries}</FieldError>
                    </Field>
                    <Field data-invalid={Boolean(validationErrors.concurrency)}>
                      <FieldLabel htmlFor="concurrency">并发线程数</FieldLabel>
                      <Input
                        id="concurrency"
                        type="number"
                        min={1}
                        max={16}
                        value={config.concurrency}
                        onChange={(event) => handleConfigChange("concurrency", Number(event.currentTarget.value))}
                        aria-invalid={Boolean(validationErrors.concurrency)}
                      />
                      <FieldError>{validationErrors.concurrency}</FieldError>
                    </Field>
                  </div>
                  <p className="-mt-2 text-xs text-muted-foreground">
                    并发线程数 &gt; 1 时，多个线程并行执行同一条命令，任一线程成功即终止其余线程；
                    最大尝试次数为所有线程累计。保活循环固定单线程。
                  </p>

                  <Field data-invalid={Boolean(validationErrors.allowedBaseUrls)}>
                    <FieldLabel htmlFor="allowedUrls">URL 拦截白名单</FieldLabel>
                    <Input
                      id="allowedUrls"
                      value={config.allowedBaseUrls}
                      onChange={(event) => handleConfigChange("allowedBaseUrls", event.currentTarget.value)}
                      placeholder="如: https://api.openai.com；多个 URL 用分号分隔"
                      aria-invalid={Boolean(validationErrors.allowedBaseUrls)}
                    />
                    <FieldError>{validationErrors.allowedBaseUrls}</FieldError>
                  </Field>

                  <Field>
                    <div className="flex flex-col gap-4 rounded-lg border bg-muted/50 p-4 sm:flex-row sm:items-center sm:justify-between">
                      <div className="flex flex-col gap-1">
                        <FieldLabel htmlFor="keepAlive" className="font-medium">
                          成功后自动保活
                        </FieldLabel>
                        <p className="text-xs text-muted-foreground">
                          仅决定下一次启动重试引擎后的初始状态，不控制当前 run。
                        </p>
                      </div>
                      <div className="flex items-center gap-3">
                        <div
                          className={cn(
                            "flex items-center gap-2 transition-opacity",
                          )}
                        >
                          <Input
                            id="keepAliveIntervalMinutes"
                            type="number"
                            min={1}
                            max={1_440}
                            value={config.keepAliveIntervalMinutes}
                            onChange={(event) =>
                              handleConfigChange("keepAliveIntervalMinutes", Number(event.currentTarget.value))
                            }
                            aria-invalid={Boolean(validationErrors.keepAliveIntervalMinutes)}
                            className="h-8 w-20"
                          />
                          <span className="whitespace-nowrap text-xs text-muted-foreground">分钟 / 次</span>
                        </div>
                        <Switch
                          id="keepAlive"
                          checked={config.keepAlive}
                          onCheckedChange={(checked) => handleConfigChange("keepAlive", checked)}
                          className="border-zinc-400 dark:border-zinc-600"
                        />
                      </div>
                    </div>
                    <FieldError>{validationErrors.keepAliveIntervalMinutes}</FieldError>
                  </Field>
                </FieldGroup>
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value="notifications" className="mt-4">
            <div className="flex flex-col gap-4">
              <Card>
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <BellIcon className="size-4" />
                    Windows 桌面通知
                  </CardTitle>
                  <CardDescription>
                    发送到 Windows 通知中心，与 Server酱独立生效，也支持计划任务运行。
                  </CardDescription>
                </CardHeader>
                <CardContent className="flex flex-col gap-5">
                  <div className="flex flex-col gap-4 rounded-lg border bg-muted/50 p-4 sm:flex-row sm:items-center sm:justify-between">
                    <div className="flex min-w-0 flex-col gap-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <p className="font-medium">本机通知状态</p>
                        <Badge variant={config.desktopNotification.enabled ? "default" : "outline"}>
                          {config.desktopNotification.enabled ? "已启用" : "已关闭"}
                        </Badge>
                      </div>
                      <p className="text-xs text-muted-foreground">
                        普通 retry 流程首次成功时，每次 run 只发送一次。
                      </p>
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      <Switch
                        id="desktopNotificationEnabled"
                        checked={config.desktopNotification.enabled}
                        onCheckedChange={setDesktopNotificationEnabled}
                      />
                      <label
                        htmlFor="desktopNotificationEnabled"
                        className="cursor-pointer text-sm font-medium"
                      >
                        启用通知
                      </label>
                    </div>
                  </div>

                  <div className="rounded-lg border bg-muted/30 p-4">
                    <p className="text-sm font-medium">触发语义</p>
                    <p className="mt-1 text-xs leading-5 text-muted-foreground">
                      Attempt 1 或后续重试首次执行成功都会通知；最终失败、停止、独立 manual
                      keep-alive 以及“本次保活”的后续 cycle 均不发送。
                    </p>
                  </div>

                  <div className="flex flex-col gap-3 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
                    <div className="min-h-5 min-w-0 text-xs text-muted-foreground">
                      {desktopNotificationLastResult || "可发送测试通知确认 Windows 系统设置"}
                    </div>
                    <Button
                      variant="outline"
                      onClick={() => void testDesktopNotification()}
                      disabled={isTestingDesktopNotification}
                      className="shrink-0"
                    >
                      {isTestingDesktopNotification ? (
                        <Spinner data-icon="inline-start" />
                      ) : (
                        <SendIcon data-icon="inline-start" />
                      )}
                      发送测试
                    </Button>
                  </div>
                </CardContent>
              </Card>

              <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <SendIcon className="size-4" />
                  个人微信通知
                </CardTitle>
                <CardDescription>
                  通过 Server酱 Turbo 发送重试流程首次成功通知。
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-6">
                <div className="flex flex-col gap-4 rounded-lg border bg-muted/50 p-4 sm:flex-row sm:items-center sm:justify-between">
                  <div className="flex min-w-0 flex-col gap-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <p className="font-medium">通知状态</p>
                      <Badge variant={serverChanStatus.configured ? "default" : "outline"}>
                        {serverChanStatus.configured ? "凭据已配置" : "凭据未配置"}
                      </Badge>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      SendKey 保存在 Windows Credential Manager，不写入应用配置。
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Switch
                      id="serverChanEnabled"
                      checked={config.serverChan.enabled}
                      onCheckedChange={setServerChanEnabled}
                    />
                    <label htmlFor="serverChanEnabled" className="cursor-pointer text-sm font-medium">
                      启用通知
                    </label>
                  </div>
                </div>

                <Field>
                  <FieldLabel htmlFor="serverChanSendKey">Server酱 SendKey</FieldLabel>
                  <div className="flex flex-col gap-2 sm:flex-row">
                    <Input
                      id="serverChanSendKey"
                      type="password"
                      autoComplete="off"
                      value={serverChanSendKey}
                      onChange={(event) => setServerChanSendKey(event.currentTarget.value)}
                      placeholder={serverChanStatus.configured ? "输入新 SendKey 可覆盖现有凭据" : "SCT..."}
                      disabled={Boolean(serverChanAction)}
                    />
                    <Button
                      onClick={() => void saveServerChanSendKey()}
                      disabled={Boolean(serverChanAction) || !serverChanSendKey.trim()}
                      className="sm:min-w-[112px]"
                    >
                      {serverChanAction === "saving" ? (
                        <Spinner data-icon="inline-start" />
                      ) : (
                        <KeyRoundIcon data-icon="inline-start" />
                      )}
                      保存凭据
                    </Button>
                  </div>
                </Field>

                <div className="rounded-lg border bg-muted/30 p-4">
                  <p className="text-sm font-medium">唯一发送条件</p>
                  <p className="mt-1 text-xs leading-5 text-muted-foreground">
                    普通 retry 流程首次出现成功结果时发送一次，无论成功发生在 Attempt 1
                    还是后续重试。最终失败、停止和独立 manual keep-alive 不发送；“本次保活”
                    的后续 cycle 不重复发送。
                  </p>
                </div>

                <div className="flex flex-col gap-3 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
                  <div className="min-h-5 min-w-0 text-xs text-muted-foreground">
                    {serverChanLastResult || "尚未发送测试通知"}
                  </div>
                  <div className="flex shrink-0 flex-wrap gap-2">
                    <Button
                      variant="outline"
                      onClick={() => void testServerChanNotification()}
                      disabled={Boolean(serverChanAction) || !serverChanStatus.configured}
                    >
                      {serverChanAction === "testing" ? (
                        <Spinner data-icon="inline-start" />
                      ) : (
                        <SendIcon data-icon="inline-start" />
                      )}
                      发送测试
                    </Button>
                    <AlertDialog>
                      <AlertDialogTrigger
                        render={
                          <Button
                            variant="destructive"
                            disabled={Boolean(serverChanAction) || !serverChanStatus.configured}
                          />
                        }
                      >
                        {serverChanAction === "deleting" ? (
                          <Spinner data-icon="inline-start" />
                        ) : (
                          <Trash2Icon data-icon="inline-start" />
                        )}
                        删除凭据
                      </AlertDialogTrigger>
                      <AlertDialogContent>
                        <AlertDialogHeader>
                          <AlertDialogMedia>
                            <KeyRoundIcon />
                          </AlertDialogMedia>
                          <AlertDialogTitle>删除 Server酱凭据？</AlertDialogTitle>
                          <AlertDialogDescription>
                            删除后通知配置仍会保留，但在重新保存 SendKey 前无法发送微信消息。
                          </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                          <AlertDialogCancel>取消</AlertDialogCancel>
                          <AlertDialogAction
                            variant="destructive"
                            onClick={() => void deleteServerChanSendKey()}
                          >
                            删除凭据
                          </AlertDialogAction>
                        </AlertDialogFooter>
                      </AlertDialogContent>
                    </AlertDialog>
                  </div>
                </div>
              </CardContent>
              </Card>
            {/* 邮件通知卡片 */}
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <MailIcon className="size-4" />
                  邮件通知
                </CardTitle>
                <CardDescription>
                  通过 SMTP 发送重试流程首次成功通知到指定邮箱。
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-6">
                {/* 状态与开关 */}
                <div className="flex flex-col gap-4 rounded-lg border bg-muted/50 p-4 sm:flex-row sm:items-center sm:justify-between">
                  <div className="flex min-w-0 flex-col gap-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <p className="font-medium">通知状态</p>
                      <Badge variant={emailStatus.configured ? "default" : "outline"}>
                        {emailStatus.configured ? "密码已配置" : "密码未配置"}
                      </Badge>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      SMTP 密码保存在 Windows Credential Manager，不写入应用配置。
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Switch
                      id="emailEnabled"
                      checked={config.email.enabled}
                      onCheckedChange={setEmailEnabled}
                    />
                    <label htmlFor="emailEnabled" className="cursor-pointer text-sm font-medium">
                      启用通知
                    </label>
                  </div>
                </div>

                {/* SMTP 配置 */}
                <div className="flex flex-col gap-4">
                  <div className="grid gap-4 sm:grid-cols-2">
                    <Field>
                      <FieldLabel htmlFor="emailSmtpHost">SMTP 服务器</FieldLabel>
                      <Input
                        id="emailSmtpHost"
                        type="text"
                        placeholder="smtp.qq.com"
                        value={config.email.smtpHost}
                        onChange={(e) => {
                          const val = e.currentTarget.value
                          setConfig((c) => ({ ...c, email: { ...c.email, smtpHost: val } }))
                        }}
                        disabled={Boolean(emailAction)}
                      />
                    </Field>
                    <Field>
                      <FieldLabel htmlFor="emailSmtpPort">端口</FieldLabel>
                      <Input
                        id="emailSmtpPort"
                        type="number"
                        placeholder="465"
                        value={config.email.smtpPort}
                        onChange={(e) => {
                          const val = Number(e.currentTarget.value)
                          setConfig((c) => ({ ...c, email: { ...c.email, smtpPort: val } }))
                        }}
                        disabled={Boolean(emailAction)}
                      />
                    </Field>
                  </div>
                  <div className="grid gap-4 sm:grid-cols-2">
                    <Field>
                      <FieldLabel htmlFor="emailSmtpUsername">发件人邮箱</FieldLabel>
                      <Input
                        id="emailSmtpUsername"
                        type="email"
                        placeholder="user@qq.com"
                        value={config.email.smtpUsername}
                        onChange={(e) => {
                          const val = e.currentTarget.value
                          setConfig((c) => ({ ...c, email: { ...c.email, smtpUsername: val } }))
                        }}
                        disabled={Boolean(emailAction)}
                      />
                    </Field>
                    <Field>
                      <FieldLabel htmlFor="emailToAddress">收件人邮箱</FieldLabel>
                      <Input
                        id="emailToAddress"
                        type="email"
                        placeholder="recipient@example.com"
                        value={config.email.toAddress}
                        onChange={(e) => {
                          const val = e.currentTarget.value
                          setConfig((c) => ({ ...c, email: { ...c.email, toAddress: val } }))
                        }}
                        disabled={Boolean(emailAction)}
                      />
                    </Field>
                  </div>
                  <Field>
                    <FieldLabel htmlFor="emailSmtpPassword">SMTP 密码 / 授权码</FieldLabel>
                    <div className="flex flex-col gap-2 sm:flex-row">
                      <Input
                        id="emailSmtpPassword"
                        type="password"
                        autoComplete="off"
                        value={emailSmtpPassword}
                        onChange={(e) => setEmailSmtpPassword(e.currentTarget.value)}
                        placeholder={emailStatus.configured ? "输入新密码可覆盖现有凭据" : "SMTP 授权码..."}
                        disabled={Boolean(emailAction)}
                      />
                      <Button
                        onClick={() => void saveEmailSmtpPassword()}
                        disabled={Boolean(emailAction) || !emailSmtpPassword.trim()}
                        className="sm:min-w-[112px]"
                      >
                        {emailAction === "saving" ? (
                          <Spinner data-icon="inline-start" />
                        ) : (
                          <KeyRoundIcon data-icon="inline-start" />
                        )}
                        保存密码
                      </Button>
                    </div>
                  </Field>
                </div>

                {/* 触发语义 */}
                <div className="rounded-lg border bg-muted/30 p-4">
                  <p className="text-sm font-medium">唯一发送条件</p>
                  <p className="mt-1 text-xs leading-5 text-muted-foreground">
                    普通 retry 流程首次出现成功结果时发送一次。最终失败、停止和
                    manual keep-alive 不发送。
                  </p>
                </div>

                {/* 底部操作栏 */}
                <div className="flex flex-col gap-3 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
                  <div className="min-h-5 min-w-0 text-xs text-muted-foreground">
                    {emailLastResult || "尚未发送测试邮件"}
                  </div>
                  <div className="flex shrink-0 flex-wrap gap-2">
                    <Button
                      variant="outline"
                      onClick={() => void testEmailNotification()}
                      disabled={Boolean(emailAction) || !emailStatus.configured}
                    >
                      {emailAction === "testing" ? (
                        <Spinner data-icon="inline-start" />
                      ) : (
                        <SendIcon data-icon="inline-start" />
                      )}
                      发送测试
                    </Button>
                    <AlertDialog>
                      <AlertDialogTrigger
                        render={
                          <Button
                            variant="destructive"
                            disabled={Boolean(emailAction) || !emailStatus.configured}
                          />
                        }
                      >
                        {emailAction === "deleting" ? (
                          <Spinner data-icon="inline-start" />
                        ) : (
                          <Trash2Icon data-icon="inline-start" />
                        )}
                        删除密码
                      </AlertDialogTrigger>
                      <AlertDialogContent>
                        <AlertDialogHeader>
                          <AlertDialogMedia>
                            <MailIcon />
                          </AlertDialogMedia>
                          <AlertDialogTitle>删除 SMTP 密码？</AlertDialogTitle>
                          <AlertDialogDescription>
                            删除后邮件配置仍会保留，但在重新保存密码前无法发送邮件通知。
                          </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                          <AlertDialogCancel>取消</AlertDialogCancel>
                          <AlertDialogAction
                            variant="destructive"
                            onClick={() => void deleteEmailSmtpPassword()}
                          >
                            删除密码
                          </AlertDialogAction>
                        </AlertDialogFooter>
                      </AlertDialogContent>
                    </AlertDialog>
                  </div>
                </div>
              </CardContent>
            </Card>
          </div>
        </TabsContent>

          <TabsContent value="logs" className="mt-4">
            <BorderGlow status={state.status} className="h-[560px]">
            <Card className="flex h-full flex-col overflow-hidden border-0 bg-zinc-950 shadow-2xl">
              {/* Terminal Header */}
              <div className="flex flex-wrap items-center justify-between gap-3 border-b border-zinc-800/80 bg-zinc-900/70 px-4 py-2.5 backdrop-blur">
                {/* Left: Window Dots & Title */}
                <div className="flex items-center gap-3">
                  <div className="flex items-center gap-1.5">
                    <span className="h-3 w-3 rounded-full bg-rose-500/80 transition-opacity hover:opacity-100" />
                    <span className="h-3 w-3 rounded-full bg-amber-500/80 transition-opacity hover:opacity-100" />
                    <span className="h-3 w-3 rounded-full bg-emerald-500/80 transition-opacity hover:opacity-100" />
                  </div>
                  <div className="h-4 w-[1px] bg-zinc-800" />
                  <div className="flex items-center gap-2">
                    <FileTerminalIcon className="size-4 text-emerald-400" />
                    <span className="font-mono text-xs font-semibold tracking-wide text-zinc-300">
                      OUTPUT STREAM
                    </span>
                    <span className="rounded-full bg-zinc-800 px-2 py-0.5 font-mono text-[10px] font-medium text-zinc-400 border border-zinc-700/50">
                      {filteredLogs.length} / {logs.length} 行
                    </span>
                  </div>
                </div>

                {/* Right: Controls & Search */}
                <div className="flex flex-wrap items-center gap-2">
                  {/* Log Filter Input */}
                  <div className="relative flex items-center">
                    <SearchIcon className="absolute left-2.5 size-3.5 text-zinc-400" />
                    <input
                      type="text"
                      value={logFilter}
                      onChange={(e) => setLogFilter(e.target.value)}
                      placeholder="检索日志..."
                      className="h-7 w-36 rounded-md border border-zinc-700 bg-zinc-900/90 pl-8 pr-7 font-mono text-xs text-zinc-100 placeholder:text-zinc-500 focus:border-emerald-500 focus:outline-none focus:ring-1 focus:ring-emerald-500 sm:w-44"
                    />
                    {logFilter ? (
                      <button
                        type="button"
                        onClick={() => setLogFilter("")}
                        className="absolute right-2 text-zinc-400 hover:text-zinc-100"
                        title="清除搜索"
                      >
                        <XIcon className="size-3" />
                      </button>
                    ) : null}
                  </div>

                  {/* Only Errors Toggle Button */}
                  <button
                    type="button"
                    onClick={() => setOnlyErrors((prev) => !prev)}
                    className={cn(
                      "flex h-7 items-center gap-1.5 rounded-md px-2.5 font-mono text-xs font-medium transition-all select-none border",
                      onlyErrors
                        ? "border-rose-400 bg-rose-600 text-white font-bold shadow-[0_0_12px_rgba(244,63,94,0.5)]"
                        : "border-rose-500/40 bg-rose-950/40 text-rose-300 hover:border-rose-500/80 hover:bg-rose-900/50 hover:text-rose-100",
                    )}
                    title="仅显示错误与异常日志"
                  >
                    <AlertTriangleIcon className={cn("size-3.5", onlyErrors ? "text-white" : "text-rose-400")} />
                    <span>只看异常</span>
                  </button>

                  {/* Auto Scroll Switch */}
                  <div className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-700 bg-zinc-900/90 px-2.5">
                    <Switch
                      id="autoscroll"
                      checked={autoScroll}
                      onCheckedChange={setAutoScroll}
                      className="scale-75 data-checked:bg-emerald-500"
                    />
                    <label
                      htmlFor="autoscroll"
                      className="cursor-pointer font-mono text-xs font-medium text-zinc-200 select-none"
                    >
                      自动滚动
                    </label>
                  </div>

                  {/* Clear Screen */}
                  <button
                    type="button"
                    onClick={clearVisibleLogs}
                    className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-700/80 bg-zinc-900/80 px-2.5 font-mono text-xs font-medium text-zinc-300 transition-colors hover:border-zinc-600 hover:bg-zinc-800 hover:text-zinc-100"
                    title="清理当前显示日志"
                  >
                    <Trash2Icon className="size-3.5 text-zinc-400" />
                    <span>清屏</span>
                  </button>

                  {/* Clear History Log Dialog */}
                  <AlertDialog open={historyDialogOpen} onOpenChange={setHistoryDialogOpen}>
                    <AlertDialogTrigger render={
                      <button
                        type="button"
                        className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-700/80 bg-zinc-900/80 px-2.5 font-mono text-xs font-medium text-zinc-300 transition-colors hover:border-zinc-600 hover:bg-zinc-800 hover:text-zinc-100"
                        title="清理历史日志文件"
                      >
                        <Trash2Icon className="size-3.5 text-zinc-400" />
                        <span>清历史</span>
                      </button>
                    } />
                    <AlertDialogContent className="border-zinc-800 bg-zinc-950 text-zinc-100">
                      <AlertDialogHeader>
                        <AlertDialogMedia>
                          <Trash2Icon className="text-rose-500" />
                        </AlertDialogMedia>
                        <AlertDialogTitle>清理历史日志？</AlertDialogTitle>
                        <AlertDialogDescription className="text-zinc-400">
                          {state.isRunning
                            ? "当前 run 正在执行，只会删除非活动日志；当前日志和 latest.log 会保留。"
                            : "将删除所有历史 run 日志，latest.log 会保留。此操作不可撤销。"}
                        </AlertDialogDescription>
                      </AlertDialogHeader>
                      <AlertDialogFooter>
                        <AlertDialogCancel className="border-zinc-800 bg-zinc-900 text-zinc-300 hover:bg-zinc-800 hover:text-zinc-100">
                          取消
                        </AlertDialogCancel>
                        <AlertDialogAction
                          variant="destructive"
                          onClick={() => void clearHistoryLogs()}
                        >
                          确认清理
                        </AlertDialogAction>
                      </AlertDialogFooter>
                    </AlertDialogContent>
                  </AlertDialog>

                  {/* Open Directory */}
                  <button
                    type="button"
                    onClick={openLogDir}
                    className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-700/80 bg-zinc-900/80 px-2.5 font-mono text-xs font-medium text-zinc-300 transition-colors hover:border-zinc-600 hover:bg-zinc-800 hover:text-zinc-100"
                    title="在系统资源管理器中打开日志文件夹"
                  >
                    <FolderOpenIcon className="size-3.5 text-zinc-400" />
                    <span>目录</span>
                  </button>
                </div>
              </div>

              {/* Log Viewport Panel */}
              <CardContent className="relative min-h-0 flex-1 bg-zinc-950 p-0">
                <GradualBlur position="top" height={18} />
                <ScrollArea className="h-full w-full terminal-scrollbar" viewportRef={scrollViewportRef}>
                  {filteredLogs.length === 0 ? (
                    <div className="flex h-full min-h-[300px] flex-col items-center justify-center gap-2 p-6 font-mono text-xs text-zinc-500 select-none">
                      <FileTerminalIcon className="size-8 text-zinc-700" />
                      <span>{logFilter || onlyErrors ? "未找到符合过滤条件的日志输出" : "等待控制台输出日志..."}</span>
                    </div>
                  ) : (
                    <div
                      className="relative w-full"
                      style={{ height: `${logVirtualizer.getTotalSize()}px` }}
                    >
                      {logVirtualizer.getVirtualItems().map((virtualRow) => {
                        const line = filteredLogs[virtualRow.index]
                        return (
                          <div
                            key={virtualRow.key}
                            ref={logVirtualizer.measureElement}
                            data-index={virtualRow.index}
                            className="absolute left-0 top-0 w-full"
                            style={{ transform: `translateY(${virtualRow.start}px)` }}
                          >
                            <FormattedLogLine
                              line={line}
                              index={virtualRow.index}
                              searchTerm={logFilter}
                            />
                          </div>
                        )
                      })}
                    </div>
                  )}
                </ScrollArea>
                <GradualBlur position="bottom" height={28} />
              </CardContent>
            </Card>
            </BorderGlow>
          </TabsContent>

          <TabsContent value="task" className="mt-4">
            <Card>
              <CardHeader>
                <CardTitle>守护进程配置</CardTitle>
                <CardDescription>注册 Windows 计划任务，让同一 headless 引擎按日执行。</CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-6">
                <div className="flex flex-col gap-4 rounded-lg border bg-muted/50 p-4 sm:flex-row sm:items-center sm:justify-between">
                  <div className="flex flex-col gap-1">
                    <p className="font-medium">当前安装状态</p>
                    <Badge variant={isTaskInstalled ? "default" : "outline"}>
                      {isTaskInstalled ? "已在系统中注册" : "未注册"}
                    </Badge>
                  </div>
                  <div className="flex gap-2">
                    <Button variant="outline" onClick={uninstallTask} disabled={!isTaskInstalled}>
                      移除任务
                    </Button>
                    <Button onClick={installTask} disabled={!configIsValid}>
                      安装/更新任务
                    </Button>
                  </div>
                </div>

                <FieldGroup>
                  <div className="grid gap-4 sm:grid-cols-2">
                    <Field data-invalid={Boolean(validationErrors.taskName)}>
                      <FieldLabel htmlFor="taskName">计划任务名称</FieldLabel>
                      <Input
                        id="taskName"
                        value={config.taskName}
                        onChange={(event) => handleConfigChange("taskName", event.currentTarget.value)}
                        aria-invalid={Boolean(validationErrors.taskName)}
                      />
                      <FieldError>{validationErrors.taskName}</FieldError>
                    </Field>
                    <Field data-invalid={Boolean(validationErrors.dailyAt)}>
                      <FieldLabel htmlFor="dailyAt">定时执行（HH:mm）</FieldLabel>
                      <Input
                        id="dailyAt"
                        value={config.dailyAt}
                        onChange={(event) => handleConfigChange("dailyAt", event.currentTarget.value)}
                        placeholder="如 03:00"
                        aria-invalid={Boolean(validationErrors.dailyAt)}
                      />
                      <FieldError>{validationErrors.dailyAt}</FieldError>
                    </Field>
                  </div>
                </FieldGroup>

                {isTaskInstalled && taskDetail ? (
                  <Field>
                    <FieldLabel>任务详细状态（Windows Task Scheduler）</FieldLabel>
                    <pre className="max-h-[200px] overflow-auto whitespace-pre-wrap rounded-md bg-muted p-4 font-mono text-xs">
                      {taskDetail}
                    </pre>
                  </Field>
                ) : null}
              </CardContent>
            </Card>
          </TabsContent>
          </Tabs>
        </AnimatedContent>
      </main>
      </div>
    </div>
  )
}

function MetricCard({
  title,
  icon,
  children,
  mono = false,
  tone = "emerald",
}: {
  title: string
  icon: React.ReactNode
  children: React.ReactNode
  mono?: boolean
  tone?: MetricTone
}) {
  const styles = metricToneStyles[tone]

  return (
    <SpotlightCard
      className="h-full min-h-[138px] p-4 sm:p-5"
      spotlightColor={styles.spotlight}
    >
      <div className={cn("absolute inset-x-6 top-0 h-px bg-gradient-to-r", styles.line)} />
      <div className="flex items-center justify-between gap-3">
        <span className="text-xs font-semibold tracking-wide text-zinc-700 dark:text-zinc-200">{title}</span>
        <span className={cn("rounded-lg border p-2 shadow-sm", styles.icon)}>{icon}</span>
      </div>
      <div className="mt-6 flex items-baseline">
        <div className={cn("text-2xl font-bold tracking-tight text-zinc-900 tabular-nums dark:text-zinc-100", mono && "font-mono")}>
          {children}
        </div>
      </div>
    </SpotlightCard>
  )
}

type MetricTone = "emerald" | "sky" | "rose" | "violet"

const metricToneStyles: Record<
  MetricTone,
  { icon: string; line: string; spotlight: string }
> = {
  emerald: {
    icon: "border-emerald-500/25 bg-emerald-500/10 text-emerald-600 dark:text-emerald-300",
    line: "from-transparent via-emerald-400/70 to-transparent",
    spotlight: "rgba(16, 185, 129, 0.15)",
  },
  sky: {
    icon: "border-sky-500/25 bg-sky-500/10 text-sky-600 dark:text-sky-300",
    line: "from-transparent via-sky-400/70 to-transparent",
    spotlight: "rgba(56, 189, 248, 0.14)",
  },
  rose: {
    icon: "border-rose-500/25 bg-rose-500/10 text-rose-600 dark:text-rose-300",
    line: "from-transparent via-rose-400/70 to-transparent",
    spotlight: "rgba(251, 113, 133, 0.13)",
  },
  violet: {
    icon: "border-violet-500/25 bg-violet-500/10 text-violet-600 dark:text-violet-300",
    line: "from-transparent via-violet-400/70 to-transparent",
    spotlight: "rgba(167, 139, 250, 0.14)",
  },
}

function statusBadge(status: "idle" | "starting" | "running" | "success" | "failed" | "stopped") {
  switch (status) {
    case "running":
      return (
        <div className="inline-flex items-center gap-2 rounded-full border border-emerald-500/40 bg-emerald-500/10 px-3 py-1 text-xs font-semibold text-emerald-600 dark:text-emerald-400 shadow-[0_0_12px_rgba(16,185,129,0.2)]">
          <span className="relative flex h-2.5 w-2.5">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
            <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500" />
          </span>
          <span>运行中</span>
        </div>
      )
    case "starting":
      return (
        <div className="inline-flex items-center gap-2 rounded-full border border-sky-500/40 bg-sky-500/10 px-3 py-1 text-xs font-semibold text-sky-600 dark:text-sky-400 shadow-[0_0_12px_rgba(56,189,248,0.2)]">
          <span className="relative flex h-2.5 w-2.5">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-sky-400 opacity-75" />
            <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-sky-500" />
          </span>
          <span>启动中...</span>
        </div>
      )
    case "success":
      return (
        <div className="inline-flex items-center gap-2 rounded-full border border-emerald-600/30 bg-emerald-500/10 px-3 py-1 text-xs font-semibold text-emerald-700 dark:text-emerald-300">
          <span className="h-2 w-2 rounded-full bg-emerald-500" />
          <span>已完成</span>
        </div>
      )
    case "failed":
      return (
        <div className="inline-flex items-center gap-2 rounded-full border border-rose-500/40 bg-rose-500/10 px-3 py-1 text-xs font-semibold text-rose-600 dark:text-rose-400 shadow-[0_0_12px_rgba(244,63,94,0.2)]">
          <span className="relative flex h-2.5 w-2.5">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-rose-400 opacity-75" />
            <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-rose-500" />
          </span>
          <span>执行失败</span>
        </div>
      )
    case "stopped":
      return (
        <div className="inline-flex items-center gap-2 rounded-full border border-amber-500/40 bg-amber-500/10 px-3 py-1 text-xs font-semibold text-amber-700 dark:text-amber-300">
          <span className="h-2 w-2 rounded-full bg-amber-500" />
          <span>已停止</span>
        </div>
      )
    default:
      return (
        <div className="inline-flex items-center gap-2 rounded-full border border-zinc-300 dark:border-zinc-700/60 bg-zinc-100 dark:bg-zinc-900 px-3 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400">
          <span className="h-2 w-2 rounded-full bg-zinc-400" />
          <span>待机 / 未运行</span>
        </div>
      )
  }
}

function firstValidationError(errors: Record<string, string | undefined>): string {
  return Object.values(errors).find(Boolean) ?? "请检查标红字段"
}

function actionErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function showActionError(title: string, error: unknown) {
  toast.error(title, { description: actionErrorMessage(error) })
}
