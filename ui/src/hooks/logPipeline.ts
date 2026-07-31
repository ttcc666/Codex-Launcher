export const MAX_LOG_LINES = 3_000
export const MIN_CATCH_UP_DELAY_MS = 50
export const HISTORY_TRUNCATED_MARKER =
  "[较早日志已省略；完整内容仍保存在磁盘日志中]"

export interface LogCursor {
  runId?: string
  byteOffset: number
}

export interface SnapshotResponse<TStatus> {
  runId: string | null
  reset: boolean
  logLines: string[]
  newByteOffset: number
  hasMore: boolean
  historyTruncated: boolean
  status: TStatus | null
}

export interface PipelineState {
  cursor: LogCursor
  logs: string[]
  appliedSequence: number
}

export interface SnapshotEnvelope<TStatus> {
  generation: number
  sequence: number
  response: SnapshotResponse<TStatus>
}

export interface ApplySnapshotResult {
  applied: boolean
  state: PipelineState
}

export function applySnapshot<TStatus>(
  current: PipelineState,
  envelope: SnapshotEnvelope<TStatus>,
  currentGeneration: number,
  maxLogLines = MAX_LOG_LINES,
): ApplySnapshotResult {
  if (
    envelope.generation !== currentGeneration ||
    envelope.sequence <= current.appliedSequence
  ) {
    return { applied: false, state: current }
  }

  const responseRunId = envelope.response.runId ?? undefined
  const shouldReset =
    envelope.response.reset || current.cursor.runId !== responseRunId
  const base = shouldReset ? [] : current.logs
  const includeHistoryMarker =
    envelope.response.historyTruncated || base.includes(HISTORY_TRUNCATED_MARKER)
  const combined = [...base, ...envelope.response.logLines].filter(
    (line) => line !== HISTORY_TRUNCATED_MARKER,
  )
  const availableLines = Math.max(0, maxLogLines - (includeHistoryMarker ? 1 : 0))
  const bounded =
    combined.length > availableLines
      ? combined.slice(combined.length - availableLines)
      : combined
  const logs = includeHistoryMarker
    ? [HISTORY_TRUNCATED_MARKER, ...bounded]
    : bounded

  return {
    applied: true,
    state: {
      cursor: {
        runId: responseRunId,
        byteOffset: envelope.response.newByteOffset,
      },
      logs,
      appliedSequence: envelope.sequence,
    },
  }
}
