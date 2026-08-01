import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react"
import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"

import {
  applySnapshot,
  MIN_CATCH_UP_DELAY_MS,
  type LogCursor,
  type PipelineState,
  type SnapshotResponse,
} from "./logPipeline"
import { isConfigValid } from "./configValidation"
import {
  errorQueueReducer,
  type QueuedError,
} from "./errorQueue"

export interface AppConfig {
  configVersion: number
  command: string
  workDir: string
  interval: number
  maxTries: number
  taskName: string
  dailyAt: string
  allowedBaseUrls: string
  keepAlive: boolean
  keepAliveIntervalMinutes: number
  desktopNotification: DesktopNotificationConfig
  serverChan: ServerChanConfig
}

export interface DesktopNotificationConfig {
  enabled: boolean
}

export interface ServerChanConfig {
  enabled: boolean
}

export interface ServerChanCredentialStatus {
  configured: boolean
}

export type RunStatus =
  | "starting"
  | "running"
  | "success"
  | "failed"
  | "stopped"

export type RunMode = "retry" | "manualKeepAlive"

export interface TaskStatus {
  runId: string
  ownerPid: number
  childPid: number | null
  status: RunStatus
  runMode: RunMode
  keepAliveEnabled: boolean
  message: string
  command: string
  workDir: string
  logFile: string
  latestLog: string
  attempt: number
  retryCount: number
  highDemandCount: number
  maxTries: number
  intervalSeconds: number
  progressPercent: number
  lastExitCode: number | null
  lastErrorSnippet: string
  resultPreview: string
  startedAt: string
  updatedAt: string
}

export interface AppViewState {
  runId?: string
  status: "idle" | RunStatus
  isRunning: boolean
  attempt: number
  highDemandCount: number
  elapsedText: string
  message: string
}

interface LogEvent {
  runId: string
}

const ACTIVE_POLL_MS = 400
const IDLE_POLL_MS = 1_500
const ERROR_POLL_MS = 2_000

const initialConfig: AppConfig = {
  configVersion: 1,
  command: "",
  workDir: "",
  interval: 10,
  maxTries: 0,
  taskName: "CodexDailyRetry0840",
  dailyAt: "08:40",
  allowedBaseUrls: "",
  keepAlive: false,
  keepAliveIntervalMinutes: 5,
  desktopNotification: {
    enabled: true,
  },
  serverChan: {
    enabled: false,
  },
}

export function useTauri() {
  const [config, setConfig] = useState<AppConfig>(initialConfig)
  const [taskStatus, setTaskStatus] = useState<TaskStatus | null>(null)
  const [logs, setLogsState] = useState<string[]>([])
  const [isTaskInstalled, setIsTaskInstalled] = useState(false)
  const [taskDetail, setTaskDetail] = useState("")
  const [isLoaded, setIsLoaded] = useState(false)
  const [errors, dispatchError] = useReducer(errorQueueReducer, [])
  const [pendingRunId, setPendingRunId] = useState<string | undefined>(undefined)
  const [clock, setClock] = useState(() => Date.now())

  const cursorRef = useRef<LogCursor>({ byteOffset: 0 })
  const logsRef = useRef<string[]>([])
  const generationRef = useRef(1)
  const sequenceRef = useRef(0)
  const appliedSequenceRef = useRef(0)
  const taskRequestSequenceRef = useRef(0)
  const localRunIdRef = useRef<string | undefined>(undefined)
  const triggerPollRef = useRef<() => void>(() => undefined)
  const errorIdRef = useRef(0)

  const replaceLogs = useCallback((next: string[]) => {
    logsRef.current = next
    setLogsState(next)
  }, [])

  const reportError = useCallback(
    (source: string, action: string, error: unknown) => {
      const queued: QueuedError = {
        id: ++errorIdRef.current,
        source,
        action,
        message: errorMessage(error),
      }
      dispatchError({ type: "enqueue", error: queued })
    },
    [],
  )

  const dismissError = useCallback((id: number) => {
    dispatchError({ type: "dismiss", id })
  }, [])

  useEffect(() => {
    invoke<AppConfig>("get_config")
      .then((loadedConfig) => {
        setConfig(loadedConfig)
        setIsLoaded(true)
      })
      .catch((error: unknown) => reportError("config", "加载配置失败", error))
  }, [reportError])

  useEffect(() => {
    if (!isLoaded || !isConfigValid(config)) return
    const timer = window.setTimeout(() => {
      invoke<void>("save_app_config", { config }).catch((error: unknown) =>
        reportError("config", "保存配置失败", error),
      )
    }, 500)
    return () => window.clearTimeout(timer)
  }, [config, isLoaded, reportError])

  useEffect(() => {
    let disposed = false
    let timer: number | undefined
    let inFlight = false
    let immediateRequested = false

    const schedule = (delay: number) => {
      if (disposed) return
      if (timer !== undefined) window.clearTimeout(timer)
      timer = window.setTimeout(poll, delay)
    }

    const requestImmediatePoll = () => {
      immediateRequested = true
      if (!inFlight) schedule(0)
    }

    const poll = async () => {
      if (disposed || inFlight) return
      inFlight = true
      immediateRequested = false
      const generation = generationRef.current
      const sequence = ++sequenceRef.current
      const request = {
        runId: cursorRef.current.runId ?? null,
        byteOffset: cursorRef.current.byteOffset,
      }

      let nextDelay = IDLE_POLL_MS
      try {
        const response = await invoke<SnapshotResponse<TaskStatus>>("get_app_state", {
          request,
        })
        if (disposed) return

        const current: PipelineState = {
          cursor: cursorRef.current,
          logs: logsRef.current,
          appliedSequence: appliedSequenceRef.current,
        }
        const applied = applySnapshot(
          current,
          { generation, sequence, response },
          generationRef.current,
        )
        if (applied.applied) {
          cursorRef.current = applied.state.cursor
          appliedSequenceRef.current = applied.state.appliedSequence
          replaceLogs(applied.state.logs)
          setTaskStatus(response.status)
          if (response.status) {
            setPendingRunId((current) =>
              response.status?.runId === current ? undefined : current,
            )
          }
        }

        if (response.hasMore) {
          nextDelay = MIN_CATCH_UP_DELAY_MS
        } else if (response.status?.status === "starting" || response.status?.status === "running") {
          nextDelay = ACTIVE_POLL_MS
        }
      } catch (error: unknown) {
        if (!disposed) reportError("snapshot", "刷新应用状态失败", error)
        nextDelay = ERROR_POLL_MS
      } finally {
        inFlight = false
        if (!disposed) schedule(immediateRequested ? 0 : nextDelay)
      }
    }

    triggerPollRef.current = requestImmediatePoll
    schedule(0)
    return () => {
      disposed = true
      triggerPollRef.current = () => undefined
      if (timer !== undefined) window.clearTimeout(timer)
    }
  }, [replaceLogs, reportError])

  useEffect(() => {
    let disposed = false
    const unlisteners: Array<() => void> = []
    const subscriptions = [
      {
        action: "订阅状态更新失败",
        promise: listen<TaskStatus>("status-update", ({ payload }) => {
          if (localRunIdRef.current === payload.runId) {
            setTaskStatus(payload)
            setPendingRunId(undefined)
            triggerPollRef.current()
          }
        }),
      },
      {
        action: "订阅日志通知失败",
        promise: listen<LogEvent>("log-line", ({ payload }) => {
          if (localRunIdRef.current === payload.runId) {
            triggerPollRef.current()
          }
        }),
      },
    ]

    for (const subscription of subscriptions) {
      void subscription.promise
        .then((unlisten) => {
          if (disposed) {
            void Promise.resolve()
              .then(() => unlisten())
              .catch((error: unknown) =>
                console.error("清理延迟完成的 Tauri listener 失败", error),
              )
          } else {
            unlisteners.push(unlisten)
          }
        })
        .catch((error: unknown) => {
          if (!disposed) reportError("events", subscription.action, error)
        })
    }

    return () => {
      disposed = true
      for (const unlisten of unlisteners) {
        void Promise.resolve()
          .then(() => unlisten())
          .catch((error: unknown) =>
            console.error("清理 Tauri listener 失败", error),
          )
      }
    }
  }, [reportError])

  useEffect(() => {
    if (taskStatus?.status !== "starting" && taskStatus?.status !== "running") return
    const timer = window.setInterval(() => setClock(Date.now()), 1_000)
    return () => window.clearInterval(timer)
  }, [taskStatus?.status])

  const beginRun = useCallback(
    (runId: string) => {
      generationRef.current += 1
      appliedSequenceRef.current = 0
      cursorRef.current = { runId, byteOffset: 0 }
      localRunIdRef.current = runId
      setPendingRunId(runId)
      setTaskStatus(null)
      replaceLogs([])
      triggerPollRef.current()
    },
    [replaceLogs],
  )

  const clearVisibleLogs = useCallback(() => {
    generationRef.current += 1
    appliedSequenceRef.current = 0
    replaceLogs([])
    triggerPollRef.current()
  }, [replaceLogs])

  const checkTask = useCallback(async () => {
    const sequence = ++taskRequestSequenceRef.current
    const taskName = config.taskName.trim()
    if (!taskName) {
      setIsTaskInstalled(false)
      setTaskDetail("")
      return
    }
    try {
      const installed = await invoke<boolean>("check_task_installed", { taskName })
      if (sequence !== taskRequestSequenceRef.current) return
      setIsTaskInstalled(installed)
      if (installed) {
        const detail = await invoke<string>("get_task_detail_command", { taskName })
        if (sequence !== taskRequestSequenceRef.current) return
        setTaskDetail(detail)
      } else {
        setTaskDetail("")
      }
    } catch (error: unknown) {
      if (sequence !== taskRequestSequenceRef.current) return
      setIsTaskInstalled(false)
      reportError("scheduler", "查询计划任务失败", error)
      setTaskDetail("")
    }
  }, [config.taskName, reportError])

  useEffect(() => {
    const timer = window.setTimeout(() => void checkTask(), 500)
    return () => window.clearTimeout(timer)
  }, [checkTask])

  const state = useMemo<AppViewState>(() => {
    const status = taskStatus?.status ?? (pendingRunId ? "starting" : "idle")
    return {
      runId: taskStatus?.runId ?? pendingRunId ?? cursorRef.current.runId,
      status,
      isRunning: status === "starting" || status === "running",
      attempt: taskStatus?.attempt ?? 0,
      highDemandCount: taskStatus?.highDemandCount ?? 0,
      elapsedText: formatElapsed(taskStatus, clock),
      message: taskStatus?.message ?? "",
    }
  }, [clock, pendingRunId, taskStatus])

  return {
    config,
    setConfig: setConfig as Dispatch<SetStateAction<AppConfig>>,
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
  }
}

function formatElapsed(status: TaskStatus | null, now: number): string {
  if (!status) return "00:00"
  const started = Date.parse(status.startedAt)
  const updated = Date.parse(status.updatedAt)
  const end = status.status === "starting" || status.status === "running" ? now : updated
  const totalSeconds = Math.max(0, Math.floor((end - started) / 1_000))
  const hours = Math.floor(totalSeconds / 3_600)
  const minutes = Math.floor((totalSeconds % 3_600) / 60)
  const seconds = totalSeconds % 60
  return hours > 0
    ? [hours, minutes, seconds].map(padTwo).join(":")
    : [minutes, seconds].map(padTwo).join(":")
}

function padTwo(value: number): string {
  return value.toString().padStart(2, "0")
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
