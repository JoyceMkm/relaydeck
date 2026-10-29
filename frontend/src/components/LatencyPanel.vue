<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import LatencyChart from './LatencyChart.vue'
import Segmented from './Segmented.vue'
import type { LatencyRange, Rule } from '../types'
import { useRuleLatency } from '../lib/queries'
import { clock, dateTime, relative } from '../lib/format'
import { latencyRanges, latencyStatusLabels, msText, percentText } from '../lib/latency'

const props = defineProps<{ rule: Rule }>()
const range = ref<LatencyRange>('1h')
watch(() => props.rule.id, () => { range.value = '1h' })
const latency = useRuleLatency(() => props.rule.id, range)

// Placeholder data from the previous range or rule must not be presented as this one.
const history = computed(() => {
  const value = latency.data.value
  return value && value.rule_id === props.rule.id && value.target_ip === props.rule.target_ip && value.target_port === props.rule.target_port ? value : null
})
const current = computed(() => history.value && !latency.isPlaceholderData.value ? history.value : null)
const supported = computed(() => history.value?.supported ?? props.rule.protocol !== 'udp')
// While a new range loads, the previous range's data keeps its own axis and labels.
const shown = computed<LatencyRange>(() => ({ 300: '24h', 3600: '7d' } as Record<number, LatencyRange>)[history.value?.step_secs ?? 0] ?? '1h')
const aggregated = computed(() => shown.value !== '1h')
const latest = computed(() => history.value?.latest ?? null)
const tiles = computed(() => {
  const summary = history.value?.summary
  const tail = shown.value === '7d'
    ? { label: '最高', value: msText(summary?.max_ms) }
    : { label: 'P99', value: msText(summary?.p99_ms) }
  return [
    { label: '平均', value: msText(summary?.avg_ms) },
    tail,
    { label: '丢包率', value: summary?.total ? percentText(summary.loss_percent) : '—', hint: summary?.total ? `${summary.lost}/${summary.total}` : '' },
  ]
})
const emptyText = computed(() => (props.rule.runtime_status === 'active' ? '等待首次探测，约一分钟内出现' : '转发运行后开始探测'))
const rows = computed(() => [...(history.value?.points ?? [])].reverse())
const rowTime = (at: number) => (shown.value === '1h' ? clock(at) : dateTime(at))
</script>

<template>
  <section aria-labelledby="latency-title" class="grid min-w-0 gap-3 border-b border-line pb-5">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <div class="min-w-0">
        <h3 id="latency-title" class="text-sm font-medium">延迟</h3>
        <p class="text-xs text-muted">中转机到落地的 TCP 握手，每分钟探测一次</p>
      </div>
      <Segmented v-if="supported" v-model="range" label="时间范围" :options="latencyRanges" size="sm" />
    </div>

    <p v-if="!supported" class="text-sm text-muted">UDP 没有可计时的握手，延迟探测仅适用于 TCP 转发。</p>
    <template v-else>
      <dl class="grid grid-cols-4 gap-2 max-sm:grid-cols-2">
        <div class="well grid content-start gap-0.5 px-3 py-2">
          <dt class="text-xs text-muted">最新</dt>
          <dd v-if="latest" class="text-base font-semibold" :class="latest.status === 'connected' ? 'text-fg' : 'text-danger'">
            {{ latest.status === 'connected' ? msText(latest.latency_ms) : latencyStatusLabels[latest.status] }}
          </dd>
          <dd v-else class="text-base font-semibold text-muted">—</dd>
          <dd v-if="latest" class="text-xs text-faint">{{ relative(latest.observed_at) }}</dd>
        </div>
        <div v-for="tile in tiles" :key="tile.label" class="well grid content-start gap-0.5 px-3 py-2">
          <dt class="text-xs text-muted">{{ tile.label }}</dt>
          <dd class="text-base font-semibold text-fg">{{ tile.value }}</dd>
          <dd v-if="tile.hint" class="text-xs text-faint tabular">{{ tile.hint }}</dd>
        </div>
      </dl>

      <div v-if="latency.isError.value && !history" class="grid h-[168px] place-items-center text-sm text-warning">延迟数据暂不可用</div>
      <div v-else-if="!history" class="skeleton h-[168px]" />
      <div v-else class="transition-opacity" :class="current ? '' : 'opacity-60'">
        <LatencyChart v-if="history.points.length" :points="history.points" :step="history.step_secs" :from="history.from" :to="history.to" :range="shown" />
        <p v-else class="grid h-[168px] place-items-center text-sm text-muted">{{ emptyText }}</p>
      </div>

      <div v-if="history?.points.length" class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted" aria-label="图例">
        <span class="inline-flex items-center gap-1.5"><span class="h-0.5 w-4 rounded-full bg-chart-line" aria-hidden="true" />{{ aggregated ? '平均延迟' : '握手延迟' }}</span>
        <span v-if="aggregated" class="inline-flex items-center gap-1.5"><span class="h-2.5 w-4 rounded-sm bg-chart-line/10" aria-hidden="true" />最小–最大</span>
        <span class="inline-flex items-center gap-1.5"><span class="h-2.5 w-[3px] rounded-full bg-chart-loss" aria-hidden="true" />丢包</span>
      </div>

      <details v-if="history?.points.length" class="group text-sm">
        <summary class="cursor-pointer text-xs text-muted hover:text-fg">数据明细</summary>
        <div class="mt-2 max-h-56 overflow-auto rounded-[10px] border border-line">
          <table class="w-full text-left text-xs">
            <thead class="sticky top-0 bg-surface text-muted">
              <tr><th class="px-3 py-1.5 font-medium">时间</th><th class="px-3 py-1.5 font-medium">{{ aggregated ? '平均' : '延迟' }}</th><th v-if="aggregated" class="px-3 py-1.5 font-medium">范围</th><th class="px-3 py-1.5 font-medium">丢包</th></tr>
            </thead>
            <tbody class="tabular">
              <tr v-for="row in rows" :key="row.at" class="border-t border-line">
                <td class="px-3 py-1.5 text-muted">{{ rowTime(row.at) }}</td>
                <td class="px-3 py-1.5">{{ row.avg_ms === null ? '全部丢包' : msText(row.avg_ms) }}</td>
                <td v-if="aggregated" class="px-3 py-1.5">{{ row.min_ms === null ? '—' : `${msText(row.min_ms)} – ${msText(row.max_ms)}` }}</td>
                <td class="px-3 py-1.5" :class="row.lost ? 'text-danger' : 'text-muted'">{{ row.lost }}/{{ row.total }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </details>
    </template>
  </section>
</template>
