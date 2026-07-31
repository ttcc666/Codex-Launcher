export const MAX_ERROR_QUEUE = 20

export interface QueuedError {
  id: number
  source: string
  action: string
  message: string
}

export type ErrorQueueAction =
  | { type: "enqueue"; error: QueuedError }
  | { type: "dismiss"; id: number }

export function errorQueueReducer(
  state: QueuedError[],
  action: ErrorQueueAction,
): QueuedError[] {
  switch (action.type) {
    case "enqueue": {
      const next = [...state, action.error]
      return next.length > MAX_ERROR_QUEUE
        ? next.slice(next.length - MAX_ERROR_QUEUE)
        : next
    }
    case "dismiss":
      return state.filter((error) => error.id !== action.id)
  }
}
