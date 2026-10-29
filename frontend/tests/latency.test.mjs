import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createServer } from 'vite'
import { createRenderer, createSSRApp, h, nextTick, reactive, ssrContextKey } from 'vue'
import { renderToString } from 'vue/server-renderer'
import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query'

test('latency history belongs to the probed rule, target and range', async t => {
  const server = await createServer({ server: { middlewareMode: true, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] }, appType: 'custom', logLevel: 'error' })
  const originalFetch = globalThis.fetch
  const originalWindow = globalThis.window
  globalThis.window = new EventTarget()
  const latency = await server.ssrLoadModule('/src/lib/latency.ts')
  const session = await server.ssrLoadModule('/src/lib/session.ts')
  const { default: Chart } = await server.ssrLoadModule('/src/components/LatencyChart.vue')
  const { default: Panel } = await server.ssrLoadModule('/src/components/LatencyPanel.vue')
  const point = (at, avg_ms, lost = 0, total = 1) => ({ at, total, lost, avg_ms, min_ms: avg_ms, max_ms: avg_ms })
  try {
    await t.test('axis ceilings are round and gaps or full losses break the line', () => {
      assert.deepEqual([0, 7, 42, 101, 260, 1800].map(latency.niceCeiling), [10, 10, 50, 200, 500, 2000])
      const runs = latency.segments([point(0, 20), point(60, 30), point(120, null, 1), point(180, 25), point(240, 28), point(600, 40)], 60)
      assert.deepEqual(runs.map(run => run.map(item => item.at)), [[0, 60], [180, 240], [600]])
      assert.equal(latency.nearestIndex([point(0, 1), point(60, 1)], 40), 1)
      assert.equal(latency.nearestIndex([], 40), -1)
      assert.deepEqual([999, 1234, 15_000, null].map(latency.msText), ['999 ms', '1.23 s', '15.0 s', '—'])
    })

    await t.test('list labels show only current probes of running rules', () => {
      const brief = { observed_at: 1000, status: 'timeout', latency_ms: null, total: 3, lost: 1 }
      assert.equal(latency.freshBrief({ runtime_status: 'active', latency: brief }, 1100), brief)
      assert.equal(latency.freshBrief({ runtime_status: 'active', latency: brief }, 1200), null)
      assert.equal(latency.freshBrief({ runtime_status: 'stopped', latency: brief }, 1100), null)
      assert.equal(latency.briefText(brief), '超时')
      assert.equal(latency.briefText({ ...brief, status: 'connected', latency_ms: 32 }), '32 ms')
    })

    await t.test('the chart draws runs, isolated points and every loss without invalid geometry', async () => {
      const points = [point(0, 20), point(60, 30), point(120, null, 1), point(180, 25), point(240, 28, 1, 5), point(600, 40)]
      const html = await renderToString(createSSRApp({ render: () => h(Chart, { points, step: 60, from: 0, to: 600, range: '1h' }) }))
      assert.equal(html.match(/class="stroke-chart-line"/g)?.length, 2)
      assert.equal(html.match(/r="3" class="fill-chart-line"/g)?.length, 1)
      assert.equal(html.match(/class="fill-chart-loss"/g)?.length, 2)
      assert.ok(!html.includes('NaN'))
      assert.ok(!html.includes('role="status"'), 'no tooltip before hover or focus')
    })

    await t.test('range switches and configuration changes never relabel older data', async () => {
      session.acceptSession({ user: { id: 1, role: 'user', username: 'tenant' }, csrf_token: 'csrf', session_ref: 'ref', mfa_required: false }, true)
      const history = (id, range, target_ip) => ({
        rule_id: id, target_ip, target_port: 443, supported: true, interval_secs: 60, step_secs: { '1h': 60, '24h': 300, '7d': 3600 }[range],
        from: 0, to: 3600, latest: { observed_at: 60, status: 'connected', latency_ms: 21 },
        summary: { total: 2, lost: 1, loss_percent: 50, avg_ms: 21, min_ms: 21, max_ms: 21, p50_ms: 21, p99_ms: 21 },
        points: [point(60, 21), point(120, null, 1)],
      })
      let pending = null
      const requests = []
      globalThis.fetch = async url => {
        const [, id, range] = String(url).match(/\/api\/rules\/(\d+)\/latency\?range=(\w+)/)
        requests.push(`${id}:${range}`)
        const body = JSON.stringify(history(Number(id), range, '8.8.8.8'))
        if (range === '24h') return new Promise(resolve => { pending = () => resolve(new Response(body)) })
        return new Response(body)
      }
      const rule = { id: 1, owner_id: 1, owner_username: 'tenant', name: 'Forward', listen_port: 62001, target_host: '8.8.8.8', target_ip: '8.8.8.8', target_port: 443, protocol: 'tcp', source_cidrs: [], enabled: true, runtime_status: 'active', runtime_updated_at: 100, updated_at: 100 }
      const props = reactive({ rule })
      const renderer = createRenderer({ createComment: () => ({}), insert() {}, remove() {}, parentNode() {}, nextSibling() {} })
      let state
      const app = renderer.createApp({ setup() { state = Panel.setup(props, { expose() {} }); return () => null } })
      const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
      app.use(VueQueryPlugin, { queryClient })
      app.provide(ssrContextKey, { modules: new Set() })
      const until = async predicate => {
        for (let attempt = 0; attempt < 200 && !predicate(); attempt++) await new Promise(resolve => setTimeout(resolve, 5))
        assert.ok(predicate())
      }
      app.mount({})
      try {
        await until(() => state.current.value)
        assert.equal(state.shown.value, '1h')
        assert.deepEqual(state.tiles.value.map(tile => tile.value), ['21 ms', '21 ms', '50%'])

        state.range.value = '24h'
        await until(() => pending)
        assert.equal(state.current.value, null, 'the previous range is only a dimmed placeholder')
        assert.equal(state.shown.value, '1h', 'placeholder points keep their own axis')
        pending()
        await until(() => state.current.value?.step_secs === 300)
        assert.equal(state.shown.value, '24h')
        assert.equal(state.tiles.value[1].label, 'P99')

        props.rule = { ...rule, target_ip: '1.1.1.1' }
        await nextTick()
        assert.equal(state.history.value, null, 'a changed target starts a new history')
        props.rule = { ...rule, id: 2 }
        await nextTick()
        assert.equal(state.range.value, '1h')
        assert.ok(state.history.value === null || state.history.value.rule_id === 2)
        await until(() => state.current.value?.rule_id === 2)
        assert.deepEqual(requests, ['1:1h', '1:24h', '2:1h'])

        props.rule = { ...rule, id: 3, protocol: 'udp' }
        await nextTick()
        assert.equal(state.supported.value, false)
      } finally {
        app.unmount()
        queryClient.clear()
      }
    })
  } finally {
    globalThis.fetch = originalFetch
    globalThis.window = originalWindow
    await server.close()
  }
})
