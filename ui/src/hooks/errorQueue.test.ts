import { expect, it } from "vitest"

import { applySnapshot, type PipelineState } from "./logPipeline"
import { errorQueueReducer, type QueuedError } from "./errorQueue"

const first: QueuedError = {
  id: 1,
  source: "config",
  action: "保存配置失败",
  message: "save failed",
}
const second: QueuedError = {
  id: 2,
  source: "scheduler",
  action: "查询计划任务失败",
  message: "query failed",
}

it("keeps concurrent errors in FIFO order across a successful snapshot", () => {
  const queued = [first, second].reduce(
    (state, error) => errorQueueReducer(state, { type: "enqueue", error }),
    [] as QueuedError[],
  )
  const pipeline: PipelineState = {
    cursor: { byteOffset: 0 },
    logs: [],
    appliedSequence: 0,
  }

  const applied = applySnapshot(
    pipeline,
    {
      generation: 1,
      sequence: 1,
      response: {
        runId: null,
        reset: false,
        logLines: [],
        newByteOffset: 0,
        hasMore: false,
        historyTruncated: false,
        status: null,
      },
    },
    1,
  )

  expect(applied.applied).toBe(true)
  expect(queued).toEqual([first, second])
})

it("dismisses one error without removing another", () => {
  const queued = [first, second]
  expect(errorQueueReducer(queued, { type: "dismiss", id: first.id })).toEqual([
    second,
  ])
})
