import type { LatencyBrief, LatencyPoint, LatencyRange, LatencyStatus, Rule } from '../types'
import { nowSeconds } from './format'

export const latencyRanges: { value: LatencyRange; label: string }[] = [
  { value: '1h', label: '1 小时' },
  { value: '24h', label: '24 小时' },
  { value: '7d', label: '7 天' },
]

export const latencyStatusLabels: Record<LatencyStatus, string> = {
  connected: '已连通', timeout: '超时', refused: '被拒绝', unreachable: '不可达', retransmit: '丢包',
}

export function msText(ms: number | null | undefined) {
  if (ms === null || ms === undefined) return '—'
  return ms >= 1000 ? `${(ms / 1000).toFixed(ms >= 10_000 ? 1 : 2)} s` : `${ms} ms`
}

export function percentText(value: number | null | undefined) {
  return value === null || value === undefined ? '—' : `${value}%`
}

/** Probes run every minute; anything older than three rounds is no longer "current". */
export function freshBrief(rule: Pick<Rule, 'runtime_status' | 'latency'>, now = nowSeconds()): LatencyBrief | null {
  const brief = rule.latency
  return rule.runtime_status === 'active' && brief && brief.observed_at >= now - 180 ? brief : null
}

export function briefText(brief: LatencyBrief) {
  return brief.status === 'connected' ? msText(brief.latency_ms) : latencyStatusLabels[brief.status]
}

/** Smallest 1/2/2.5/5 × 10ⁿ ceiling, so axis ticks are round numbers. */
export function niceCeiling(value: number) {
  if (!(value > 0)) return 10
  const magnitude = 10 ** Math.floor(Math.log10(value))
  const step = [1, 2, 2.5, 5, 10].find(candidate => candidate * magnitude >= value)!
  return step * magnitude
}

/** Runs of consecutive measured points; a missing interval or a fully lost point breaks the line. */
export function segments(points: LatencyPoint[], step: number) {
  const runs: LatencyPoint[][] = []
  let current: LatencyPoint[] = []
  let previous: LatencyPoint | null = null
  for (const point of points) {
    const gap = previous !== null && point.at - previous.at > step * 1.5
    if (point.avg_ms === null || gap) {
      if (current.length) runs.push(current)
      current = []
    }
    if (point.avg_ms !== null) current.push(point)
    previous = point
  }
  if (current.length) runs.push(current)
  return runs
}

/** Index of the point closest to `at`, or -1 for no points. */
export function nearestIndex(points: LatencyPoint[], at: number) {
  let best = -1
  let distance = Infinity
  points.forEach((point, index) => {
    const delta = Math.abs(point.at - at)
    if (delta < distance) { best = index; distance = delta }
  })
  return best
}
