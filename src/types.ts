export type ProviderId = 'openai' | 'anthropic'
export type ProviderStatus =
  'fresh' | 'stale' | 'unavailable' | 'authentication_required' | 'rate_limited' | 'unsupported'

export interface UsageWindow {
  id: string
  label: string
  usedPercent: number | null
  remainingPercent: number | null
  resetAt: number | null
}

export interface ProviderUsage {
  provider: ProviderId
  windows: UsageWindow[]
  lastUpdated: number | null
  source: string
  status: ProviderStatus
  error: string | null
}

export interface UsageSnapshot {
  providers: ProviderUsage[]
  refreshedAt: number
}

export interface AppSettings {
  launchAtStartup: boolean
  alwaysOnTop: boolean
  refreshIntervalMinutes: number
  notifications: boolean
  compactMode: boolean
  showResetCountdown: boolean
  showLongWindow: boolean
  firstRunComplete: boolean
}
