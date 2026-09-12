import type { ProviderUsage, UsageWindow } from '../types'

export function percentage(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return '—'
  return `${Math.round(Math.min(100, Math.max(0, value)))}%`
}

export function compactWindows(provider: ProviderUsage, showLongWindow = true): UsageWindow[] {
  const windows = provider.windows.slice(0, showLongWindow ? 2 : 1)
  return windows.length ? windows : [emptyWindow('short'), emptyWindow('long')].slice(0, showLongWindow ? 2 : 1)
}

function emptyWindow(id: string): UsageWindow {
  return { id, label: '', usedPercent: null, remainingPercent: null, resetAt: null }
}

export function countdown(resetAt: number | null, now = Date.now() / 1000): string {
  if (!resetAt) return '—'
  const seconds = Math.max(0, Math.floor(resetAt - now))
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  return `${hours.toString().padStart(2, '0')}h ${minutes.toString().padStart(2, '0')}m`
}

export function resetTime(resetAt: number | null): string {
  if (!resetAt) return '—'
  return new Intl.DateTimeFormat(undefined, {
    weekday: 'short',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(resetAt * 1000))
}

export function updatedTime(epoch: number | null): string {
  if (!epoch) return '—'
  return new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' }).format(
    new Date(epoch * 1000),
  )
}

export function quotaTone(value: number | null): 'neutral' | 'warning' | 'danger' | 'exhausted' {
  if (value === null || value > 25) return 'neutral'
  if (value === 0) return 'exhausted'
  if (value < 10) return 'danger'
  return 'warning'
}
