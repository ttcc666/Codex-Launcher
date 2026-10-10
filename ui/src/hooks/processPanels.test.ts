import { describe, expect, it } from "vitest"

import { buildProcessPanels } from "./processPanels"

describe("buildProcessPanels", () => {
  it("replaces the previous attempt panel when a new attempt starts on the same worker", () => {
    const logs = [
      "[2026-10-10 14:49:06] [launcher] 开始执行: codex exec 分析1+1",
      "[2026-10-10 14:49:06] [launcher] 工作目录: D:\\Local",
      "[2026-10-10 14:49:06] [launcher] 第 1 次尝试执行中",
      "[2026-10-10 14:49:07] [stdout] attempt-1-output",
      "[2026-10-10 14:49:08] [launcher] 命令异常退出 (exit=1)，1 秒后重试",
      "[2026-10-10 14:49:09] [launcher] 第 2 次尝试执行中",
      "[2026-10-10 14:49:10] [stdout] attempt-2-output",
    ]

    const view = buildProcessPanels(logs)

    expect(view.bannerLines).toEqual([
      "[14:49:06] 开始执行: codex exec 分析1+1",
      "[14:49:06] 工作目录: D:\\Local",
    ])
    expect(view.panels).toHaveLength(1)
    expect(view.panels[0].attempt).toBe(2)
    expect(view.panels[0].status).toBe("running")
    expect(view.panels[0].lines.join("\n")).toContain("attempt-2-output")
    expect(view.panels[0].lines.join("\n")).not.toContain("attempt-1-output")
  })

  it("isolates concurrent workers into separate panels and auto-closes cancelled workers", () => {
    const logs = [
      "[2026-10-10 14:49:06] [launcher] 开始执行: codex exec 分析1+1",
      "[2026-10-10 14:49:06] [launcher] 并发线程数: 3（任一线程成功即终止其余线程）",
      "[2026-10-10 14:49:06] [w1] [launcher] 第 1 次尝试执行中",
      "[2026-10-10 14:49:06] [w2] [launcher] 第 2 次尝试执行中",
      "[2026-10-10 14:49:06] [w3] [launcher] 第 3 次尝试执行中",
      "[2026-10-10 14:49:07] [w1] [stdout] w1-line",
      "[2026-10-10 14:49:07] [w2] [stdout] w2-winner-line",
      "[2026-10-10 14:49:07] [w3] [stderr] w3-error-line",
    ]

    const activeView = buildProcessPanels(logs)
    expect(activeView.isMultiWorker).toBe(true)
    expect(activeView.panels).toHaveLength(3)
    expect(activeView.panels.map((p) => p.workerId)).toEqual([1, 2, 3])
    expect(activeView.panels[0].lines.join("\n")).not.toContain("[w1]")

    const finishedLogs = [
      ...logs,
      "[2026-10-10 14:49:08] [w1] [launcher] 已被其它并发线程抢先完成，本线程已取消",
      "[2026-10-10 14:49:08] [w3] [launcher] 已被其它并发线程抢先完成，本线程已取消",
      "[2026-10-10 14:49:08] [launcher] 命令成功完成 (exit=0)",
    ]

    const finalView = buildProcessPanels(finishedLogs)
    expect(finalView.panels).toHaveLength(1)
    expect(finalView.panels[0].workerId).toBe(2)
    expect(finalView.panels[0].status).toBe("succeeded")
    expect(finalView.panels[0].lines.join("\n")).toContain("w2-winner-line")
  })

  it("retains the final round panels when the task stops or fails", () => {
    const logs = [
      "[2026-10-10 14:49:06] [w1] [launcher] 第 1 次尝试执行中",
      "[2026-10-10 14:49:07] [w1] [stderr] ERROR: Reconnecting... 1/5",
      "[2026-10-10 14:49:07] [w1] [stderr] warning: Codex is ignoring 1 unrecognized configuration setting.",
      "[2026-10-10 14:49:07] [w1] [stderr] user (C:\\Users\\test\\.codex\\config.toml): `disable_response_storage` is ignored.",
    ]

    const runningView = buildProcessPanels(logs)
    expect(runningView.panels[0].lastSummary).toBe("Reconnecting... 1/5")

    const view = buildProcessPanels([
      ...logs,
      "[2026-10-10 14:49:08] [launcher] 用户停止了任务",
    ])
    expect(view.panels).toHaveLength(1)
    expect(view.panels[0].status).toBe("stopped")
    expect(view.panels[0].lastSummary).toBe("用户停止了任务")
    expect(view.panels[0].lines.join("\n")).toContain("用户停止了任务")
  })
})
