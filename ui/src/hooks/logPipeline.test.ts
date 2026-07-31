import { describe, expect, it } from "vitest"

import {
  applySnapshot,
  HISTORY_TRUNCATED_MARKER,
  MIN_CATCH_UP_DELAY_MS,
  type PipelineState,
} from "./logPipeline"

const initial: PipelineState = {
  cursor: { runId: "run-a", byteOffset: 100 },
  logs: ["a-1"],
  appliedSequence: 1,
}

it("resets logs on run switch without losing the first line", () => {
  const result = applySnapshot(
    initial,
    {
      generation: 1,
      sequence: 2,
      response: {
        runId: "run-b",
        reset: true,
        logLines: ["b-first", "b-second"],
        newByteOffset: 42,
        hasMore: false,
        historyTruncated: false,
        status: null,
      },
    },
    1,
  )

  expect(result.applied).toBe(true)
  expect(result.state.cursor).toEqual({ runId: "run-b", byteOffset: 42 })
  expect(result.state.logs).toEqual(["b-first", "b-second"])
})

describe("late response protection", () => {
  it("ignores a response from an invalidated generation", () => {
    const result = applySnapshot(
      initial,
      {
        generation: 1,
        sequence: 99,
        response: {
          runId: "run-a",
          reset: false,
          logLines: ["stale"],
          newByteOffset: 999,
          hasMore: false,
          historyTruncated: false,
          status: null,
        },
      },
      2,
    )

    expect(result.applied).toBe(false)
    expect(result.state).toBe(initial)
  })

  it("does not let an older sequence roll back the cursor or duplicate logs", () => {
    const newer = applySnapshot(
      initial,
      {
        generation: 1,
        sequence: 3,
        response: {
          runId: "run-a",
          reset: false,
          logLines: ["a-2"],
          newByteOffset: 200,
          hasMore: false,
          historyTruncated: false,
          status: null,
        },
      },
      1,
    ).state
    const older = applySnapshot(
      newer,
      {
        generation: 1,
        sequence: 2,
        response: {
          runId: "run-a",
          reset: false,
          logLines: ["a-2"],
          newByteOffset: 150,
          hasMore: false,
          historyTruncated: false,
          status: null,
        },
      },
      1,
    )

    expect(older.applied).toBe(false)
    expect(older.state.cursor.byteOffset).toBe(200)
    expect(older.state.logs).toEqual(["a-1", "a-2"])
  })
})

it("keeps the React log buffer bounded", () => {
  const result = applySnapshot(
    { cursor: { byteOffset: 0 }, logs: [], appliedSequence: 0 },
    {
      generation: 1,
      sequence: 1,
      response: {
        runId: "run-a",
        reset: true,
        logLines: Array.from({ length: 50 }, (_, index) => `line-${index}`),
        newByteOffset: 1_000,
        hasMore: false,
        historyTruncated: false,
        status: null,
      },
    },
    1,
    10,
  )

  expect(result.state.logs).toHaveLength(10)
  expect(result.state.logs[0]).toBe("line-40")
})

it("inserts the truncated-history marker only once", () => {
  const first = applySnapshot(
    { cursor: { byteOffset: 0 }, logs: [], appliedSequence: 0 },
    {
      generation: 1,
      sequence: 1,
      response: {
        runId: "run-a",
        reset: true,
        logLines: ["tail-1"],
        newByteOffset: 100,
        hasMore: true,
        historyTruncated: true,
        status: null,
      },
    },
    1,
  ).state
  const second = applySnapshot(
    first,
    {
      generation: 1,
      sequence: 2,
      response: {
        runId: "run-a",
        reset: false,
        logLines: ["tail-2"],
        newByteOffset: 200,
        hasMore: false,
        historyTruncated: false,
        status: null,
      },
    },
    1,
  ).state

  expect(second.logs).toEqual([HISTORY_TRUNCATED_MARKER, "tail-1", "tail-2"])
  expect(second.logs.filter((line) => line === HISTORY_TRUNCATED_MARKER)).toHaveLength(1)
})

it("uses a nonzero minimum delay for catch-up polling", () => {
  expect(MIN_CATCH_UP_DELAY_MS).toBeGreaterThan(0)
})
