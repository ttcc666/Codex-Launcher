import { HISTORY_TRUNCATED_MARKER } from "./logPipeline"

export type ProcessPanelStatus =
  | "running"
  | "retrying"
  | "keepAliveWait"
  | "succeeded"
  | "failed"
  | "stopped"

export interface ProcessPanel {
  id: string
  workerId: number
  attempt: number
  status: ProcessPanelStatus
  startedAt: string
  statusText: string
  lastSummary: string
  lines: string[]
}

export interface ProcessPanelsView {
  bannerLines: string[]
  panels: ProcessPanel[]
  isMultiWorker: boolean
}

const LOG_RECORD_REGEX =
  /^\[(\d{4}-\d{2}-\d{2}\s+(\d{2}:\d{2}:\d{2}(?:\.\d+)?))\]\s+(?:\[w(\d+)\]\s+)?\[(launcher|stdout|stderr)\]\s?(.*)$/

const ATTEMPT_START_REGEX = /^第\s*(\d+)\s*次尝试执行中/

export function buildProcessPanels(logs: string[]): ProcessPanelsView {
  const bannerLines: string[] = []
  const panelsByWorker = new Map<number, ProcessPanel>()
  let isMultiWorker = false
  let lastActiveWorkerId = 1

  for (const rawLine of logs) {
    if (rawLine === HISTORY_TRUNCATED_MARKER) {
      continue
    }

    const match = rawLine.match(LOG_RECORD_REGEX)
    if (!match) {
      const target = getOrCreateFallbackPanel(panelsByWorker, lastActiveWorkerId, "")
      target.lines.push(rawLine)
      updateLastSummary(target, rawLine)
      continue
    }

    const fullTimestamp = match[1]
    const shortTime = match[2]
    const workerIdRaw = match[3]
    const source = match[4] as "launcher" | "stdout" | "stderr"
    const text = match[5]

    if (workerIdRaw !== undefined) {
      isMultiWorker = true
    }

    // 全局启动元数据行放入顶部概览栏，不混入子进程终端
    if (workerIdRaw === undefined && source === "launcher" && isGlobalBannerMessage(text)) {
      if (text.startsWith("并发线程数:")) {
        isMultiWorker = true
      }
      bannerLines.push(`[${shortTime}] ${text}`)
      continue
    }

    const workerId = workerIdRaw !== undefined ? Number(workerIdRaw) : 1
    lastActiveWorkerId = workerId

    if (source === "launcher") {
      const attemptMatch = text.match(ATTEMPT_START_REGEX)
      if (attemptMatch) {
        const attempt = Number(attemptMatch[1])
        // 开启新一轮尝试：自动关闭并替换该线程上一轮已结束的进程面板
        const nextPanel: ProcessPanel = {
          id: `w${workerId}-attempt-${attempt}`,
          workerId,
          attempt,
          status: "running",
          startedAt: shortTime,
          statusText: `第 ${attempt} 次尝试执行中`,
          lastSummary: "正在连接并执行...",
          lines: [formatPanelLine(fullTimestamp, source, text)],
        }
        panelsByWorker.set(workerId, nextPanel)
        continue
      }

      if (text.includes("已被其它并发线程抢先完成，本线程已取消")) {
        // 并发抢跑中落败被取消的线程面板立即自动关闭
        panelsByWorker.delete(workerId)
        continue
      }

      if (text.includes("保活已开启")) {
        const panel = getOrCreateFallbackPanel(panelsByWorker, workerId, shortTime)
        panel.status = "keepAliveWait"
        panel.statusText = text
        panel.lastSummary = text
        panel.lines.push(formatPanelLine(fullTimestamp, source, text))
        // 进入单线程保活阶段，关闭其余并发线程的旧面板
        for (const key of Array.from(panelsByWorker.keys())) {
          if (key !== workerId) {
            panelsByWorker.delete(key)
          }
        }
        continue
      }

      if (text.includes("后重试")) {
        const panel = getOrCreateFallbackPanel(panelsByWorker, workerId, shortTime)
        panel.status = "retrying"
        panel.statusText = text
        panel.lastSummary = text
        panel.lines.push(formatPanelLine(fullTimestamp, source, text))
        continue
      }

      // 全局终态消息（无 workerId）
      if (workerIdRaw === undefined && isTerminalMessage(text)) {
        applyTerminalMessage(panelsByWorker, lastActiveWorkerId, fullTimestamp, shortTime, text)
        continue
      }
    }

    const panel = getOrCreateFallbackPanel(panelsByWorker, workerId, shortTime)
    panel.lines.push(formatPanelLine(fullTimestamp, source, text))
    updateLastSummary(panel, text)
  }

  const panels = Array.from(panelsByWorker.values()).sort(
    (left, right) => left.workerId - right.workerId,
  )

  return {
    bannerLines,
    panels,
    isMultiWorker: isMultiWorker || panels.length > 1,
  }
}

function updateLastSummary(panel: ProcessPanel, rawText: string) {
  const trimmed = rawText.trim()
  if (!trimmed) return
  if (
    trimmed.startsWith("warning: Codex is ignoring") ||
    trimmed.includes("is ignored.") ||
    trimmed.startsWith("deprecated settings.")
  ) {
    return
  }
  panel.lastSummary = trimmed.replace(/^ERROR:\s*/i, "")
}

function isGlobalBannerMessage(text: string): boolean {
  return (
    text.startsWith("开始执行:") ||
    text.startsWith("工作目录:") ||
    text.startsWith("并发线程数:") ||
    text.startsWith("运行模式:")
  )
}

function isTerminalMessage(text: string): boolean {
  return (
    text.startsWith("命令成功完成") ||
    text.startsWith("保活已关闭") ||
    text.startsWith("用户停止了任务") ||
    text.startsWith("用户在") ||
    text.startsWith("收到匹配 run ID 的远程停止请求") ||
    text.startsWith("命令未成功且已达到最大尝试次数") ||
    text.startsWith("启动子进程失败") ||
    text.startsWith("子进程异常终止")
  )
}

function applyTerminalMessage(
  panelsByWorker: Map<number, ProcessPanel>,
  fallbackWorkerId: number,
  fullTimestamp: string,
  shortTime: string,
  text: string,
) {
  const formatted = formatPanelLine(fullTimestamp, "launcher", text)
  const isSuccess = text.startsWith("命令成功完成") || text.startsWith("保活已关闭")
  const isStopped =
    text.startsWith("用户停止了任务") ||
    text.startsWith("用户在") ||
    text.startsWith("收到匹配 run ID 的远程停止请求")

  if (panelsByWorker.size === 0) {
    const status: ProcessPanelStatus = isSuccess
      ? "succeeded"
      : isStopped
        ? "stopped"
        : "failed"
    panelsByWorker.set(fallbackWorkerId, {
      id: `w${fallbackWorkerId}-terminal`,
      workerId: fallbackWorkerId,
      attempt: 1,
      status,
      startedAt: shortTime,
      statusText: text,
      lastSummary: text,
      lines: [formatted],
    })
    return
  }

  if (isSuccess) {
    // 成功结束时：若有某个 running/keepAliveWait 面板则保留该胜出面板，关闭其余未成功面板
    const winner =
      Array.from(panelsByWorker.values()).find(
        (panel) => panel.status === "running" || panel.status === "keepAliveWait",
      ) ?? Array.from(panelsByWorker.values())[panelsByWorker.size - 1]
    winner.status = "succeeded"
    winner.statusText = text
    winner.lastSummary = text
    winner.lines.push(formatted)
    panelsByWorker.clear()
    panelsByWorker.set(winner.workerId, winner)
    return
  }

  const nextStatus: ProcessPanelStatus = isStopped ? "stopped" : "failed"
  for (const panel of panelsByWorker.values()) {
    panel.status = nextStatus
    panel.statusText = text
    panel.lastSummary = text
    panel.lines.push(formatted)
  }
}

function getOrCreateFallbackPanel(
  panelsByWorker: Map<number, ProcessPanel>,
  workerId: number,
  shortTime: string,
): ProcessPanel {
  const existing = panelsByWorker.get(workerId)
  if (existing) {
    return existing
  }
  const created: ProcessPanel = {
    id: `w${workerId}-attempt-1`,
    workerId,
    attempt: 1,
    status: "running",
    startedAt: shortTime,
    statusText: "执行中",
    lastSummary: "正在连接并执行...",
    lines: [],
  }
  panelsByWorker.set(workerId, created)
  return created
}

function formatPanelLine(
  fullTimestamp: string,
  source: "launcher" | "stdout" | "stderr",
  text: string,
): string {
  return `[${fullTimestamp}] [${source}] ${text}`
}
