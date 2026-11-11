import { useMemo } from 'react'
import { useSystemMetrics } from './useSystemMetrics'
import { useNetwork } from './useNetwork'

export type BallState = 'normal' | 'perf' | 'battery' | 'offline'

export interface BallHealth {
  state: BallState
  /** battery percentage when available, null otherwise */
  batteryPct: number | null
  /** 是否在充电（含无电池台式机之外的充电态；无电池时 batteryPct 为 null） */
  charging: boolean
  cpu: number
  mem: number
  online: boolean
}

const BATTERY_LOW = 20
const CPU_HIGH = 80
const MEM_HIGH = 85

/**
 * Derives the ball's alert state from live system metrics.
 * Priority (highest wins): battery-red blink > perf-orange > offline-xx > normal.
 */
export function useBallHealth(): BallHealth {
  // useSystemMetrics 返回 { metrics, history } 包装对象，metrics 本身可空
  const { metrics } = useSystemMetrics()
  const { status } = useNetwork()

  return useMemo(() => {
    const cpu = metrics?.cpu ?? 0
    const mem =
      metrics && metrics.memTotal > 0 ? (metrics.memUsed / metrics.memTotal) * 100 : 0
    const batteryPct = metrics?.battery?.charge ?? null
    // status 为 null = 尚未收到首个网络事件（启动 ~1s 内），按在线处理，避免开屏误闪 XX 眼
    const online = status === null ? true : status.online
    // 充电中永不红闪（sir 2026-10-30：插电不闪，改用闪电标；无电池时 batteryPct 为 null 也不会红闪）
    const charging = metrics?.battery?.state === 'charging'

    // Priority chain, evaluated low→high so the highest priority wins
    let state: BallState = 'normal'
    if (!online) state = 'offline'
    if (cpu > CPU_HIGH || mem > MEM_HIGH) state = 'perf'
    if (!charging && batteryPct !== null && batteryPct < BATTERY_LOW) state = 'battery'

    return { state, batteryPct, charging, cpu, mem, online }
  }, [metrics, status])
}
