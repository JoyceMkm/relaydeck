<script setup lang="ts">
import { computed, ref } from 'vue'
import { useElementSize } from '@vueuse/core'
import type { LatencyPoint, LatencyRange } from '../types'
import { clock } from '../lib/format'
import { msText, nearestIndex, niceCeiling, segments } from '../lib/latency'

const props = defineProps<{ points: LatencyPoint[]; step: number; from: number; to: number; range: LatencyRange }>()

const container = ref<HTMLElement | null>(null)
const { width: measured } = useElementSize(container)
// Server rendering and the first frame have no layout yet.
const width = computed(() => Math.max(240, Math.round(measured.value) || 560))
const height = 168
const plot = { top: 10, bottom: 140, left: 52, right: 10 }
const plotRight = computed(() => width.value - plot.right)

const aggregated = computed(() => props.range !== '1h')
const ceiling = computed(() => niceCeiling(Math.max(0, ...props.points.map(point => (aggregated.value ? point.max_ms : point.avg_ms) ?? 0)) * 1.1))
const x = (at: number) => plot.left + ((at - props.from) / Math.max(1, props.to - props.from)) * (plotRight.value - plot.left)
const y = (ms: number) => plot.bottom - (ms / ceiling.value) * (plot.bottom - plot.top)
const yTicks = computed(() => [0, ceiling.value / 2, ceiling.value].map(value => ({ value, y: y(value), label: value === 0 ? '0' : msText(Math.round(value)) })))
const xTicks = computed(() => {
  const format = props.range === '7d'
    ? (value: number) => new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit' }).format(new Date(value * 1000))
    : clock
  return [0, 1, 2, 3].map(index => {
    const at = props.from + ((props.to - props.from) * index) / 3
    return { at, x: x(at), label: format(at), anchor: index === 0 ? 'start' : index === 3 ? 'end' : 'middle' }
  })
})

const runs = computed(() => segments(props.points, props.step))
const lines = computed(() => runs.value.filter(run => run.length > 1).map(run => run.map((point, index) => `${index ? 'L' : 'M'}${x(point.at).toFixed(1)},${y(point.avg_ms!).toFixed(1)}`).join('')))
// Min–max range of aggregated points, drawn as a wash behind the average line.
const bands = computed(() => aggregated.value ? runs.value.filter(run => run.length > 1).map(run => {
  const upper = run.map((point, index) => `${index ? 'L' : 'M'}${x(point.at).toFixed(1)},${y(point.max_ms ?? point.avg_ms!).toFixed(1)}`).join('')
  const lower = [...run].reverse().map(point => `L${x(point.at).toFixed(1)},${y(point.min_ms ?? point.avg_ms!).toFixed(1)}`).join('')
  return `${upper}${lower}Z`
}) : [])
// Isolated measurements have no neighbour to draw a line to.
const singles = computed(() => runs.value.filter(run => run.length === 1).map(run => run[0]))
const losses = computed(() => props.points.filter(point => point.lost > 0).map(point => {
  const size = 4 + 10 * (point.lost / point.total)
  return { at: point.at, x: x(point.at) - 1.5, y: plot.bottom - size, height: size }
}))

const active = ref(-1)
const activePoint = computed(() => props.points[active.value] ?? null)
function onPointer(event: PointerEvent) {
  const box = (event.currentTarget as SVGElement).getBoundingClientRect()
  const ratio = (event.clientX - box.left) / Math.max(1, box.width)
  const left = plot.left / width.value
  const right = plotRight.value / width.value
  const at = props.from + ((ratio - left) / (right - left)) * (props.to - props.from)
  active.value = nearestIndex(props.points, at)
}
function onKeydown(event: KeyboardEvent) {
  const last = props.points.length - 1
  if (last < 0) return
  const moves: Record<string, number> = { ArrowLeft: Math.max(0, active.value - 1), ArrowRight: Math.min(last, active.value + 1), Home: 0, End: last }
  if (event.key === 'Escape') { active.value = -1; return }
  if (!(event.key in moves)) return
  event.preventDefault()
  active.value = active.value < 0 ? last : moves[event.key]
}
const tooltipLeft = computed(() => activePoint.value ? Math.min(Math.max(x(activePoint.value.at), 96), width.value - 96) : 0)
const pointTime = (at: number) => props.range === '7d'
  ? new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(new Date(at * 1000))
  : clock(at)
const describe = computed(() => `延迟趋势，纵轴 0 到 ${msText(ceiling.value)}。用左右方向键逐点查看。`)
defineExpose({ active, activePoint, lines, losses, singles })
</script>

<template>
  <!-- The SVG is positioned out of flow, so its width never feeds back into the measured width. -->
  <div ref="container" class="relative w-full min-w-0 select-none" :style="{ height: `${height}px` }">
    <svg
      :width="width" :height="height" :viewBox="`0 0 ${width} ${height}`" class="absolute inset-0 block overflow-visible outline-none"
      role="img" tabindex="0" :aria-label="describe"
      @pointermove="onPointer" @pointerleave="active = -1" @focus="active = points.length - 1" @blur="active = -1" @keydown="onKeydown"
    >
      <g aria-hidden="true">
        <g v-for="tick in yTicks" :key="tick.value">
          <line :x1="plot.left" :x2="plotRight" :y1="tick.y" :y2="tick.y" class="stroke-line" stroke-width="1" shape-rendering="crispEdges" />
          <text :x="plot.left - 8" :y="tick.y" dy="0.32em" text-anchor="end" class="fill-faint text-[11px] tabular">{{ tick.label }}</text>
        </g>
        <text v-for="tick in xTicks" :key="tick.at" :x="tick.x" :y="height - 6" :text-anchor="tick.anchor" class="fill-faint text-[11px] tabular">{{ tick.label }}</text>
        <path v-for="(band, index) in bands" :key="`band-${index}`" :d="band" class="fill-chart-line opacity-10" />
        <path v-for="(line, index) in lines" :key="`line-${index}`" :d="line" class="stroke-chart-line" fill="none" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" />
        <circle v-for="point in singles" :key="`single-${point.at}`" :cx="x(point.at)" :cy="y(point.avg_ms!)" r="3" class="fill-chart-line" />
        <rect v-for="loss in losses" :key="`loss-${loss.at}`" :x="loss.x" :y="loss.y" width="3" :height="loss.height" rx="1.5" class="fill-chart-loss" />
        <template v-if="activePoint">
          <line :x1="x(activePoint.at)" :x2="x(activePoint.at)" :y1="plot.top" :y2="plot.bottom" class="stroke-line-strong" stroke-width="1" shape-rendering="crispEdges" />
          <circle v-if="activePoint.avg_ms !== null" :cx="x(activePoint.at)" :cy="y(activePoint.avg_ms)" r="4" class="fill-chart-line stroke-surface" stroke-width="2" />
        </template>
      </g>
    </svg>
    <div
      v-if="activePoint" class="pop pointer-events-none absolute top-0 z-10 -translate-x-1/2 !px-2.5 !py-1.5 text-xs whitespace-nowrap"
      :style="{ left: `${tooltipLeft}px` }" role="status"
    >
      <p class="text-sm font-semibold text-fg tabular">{{ activePoint.avg_ms === null ? '全部丢包' : msText(activePoint.avg_ms) }}</p>
      <p v-if="aggregated && activePoint.min_ms !== null && activePoint.max_ms !== activePoint.min_ms" class="text-muted tabular">范围 {{ msText(activePoint.min_ms) }} – {{ msText(activePoint.max_ms) }}</p>
      <p v-if="activePoint.lost" class="flex items-center gap-1.5 text-muted tabular"><span class="inline-block h-2.5 w-[3px] rounded-full bg-chart-loss" aria-hidden="true" />丢包 {{ activePoint.lost }}/{{ activePoint.total }}</p>
      <p class="text-faint tabular">{{ pointTime(activePoint.at) }}</p>
    </div>
  </div>
</template>
