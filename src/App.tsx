import { invoke } from '@tauri-apps/api/core'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { ProviderLogo } from './components/ProviderLogo'
import {
  compactWindows,
  countdown,
  percentage,
  quotaTone,
  resetTime,
  updatedTime,
} from './lib/format'
import type { AppSettings, ProviderId, ProviderUsage, UsageSnapshot } from './types'

const EMPTY: UsageSnapshot = {
  providers: [emptyProvider('openai'), emptyProvider('anthropic')],
  refreshedAt: 0,
}

function emptyProvider(provider: ProviderId): ProviderUsage {
  return {
    provider,
    windows: [],
    lastUpdated: null,
    source: '',
    status: 'unavailable',
    error: null,
  }
}

export default function App() {
  const [snapshot, setSnapshot] = useState<UsageSnapshot>(EMPTY)
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [surface, setSurface] = useState<'compact' | 'detail' | 'settings' | 'onboarding'>(
    'compact',
  )
  const [refreshing, setRefreshing] = useState(false)

  const load = useCallback(async () => {
    const [nextSnapshot, nextSettings] = await Promise.all([
      invoke<UsageSnapshot>('get_snapshot'),
      invoke<AppSettings>('get_settings'),
    ])
    setSnapshot(nextSnapshot)
    setSettings(nextSettings)
    if (!nextSettings.firstRunComplete) {
      setSurface('onboarding')
      await invoke('set_surface', { surface: 'onboarding' })
    } else if (!nextSettings.compactMode) {
      setSurface('detail')
      await invoke('set_surface', { surface: 'detail' })
    }
  }, [])

  useEffect(() => {
    void load()
    const timer = window.setInterval(
      () => void invoke<UsageSnapshot>('get_snapshot').then(setSnapshot),
      15_000,
    )
    return () => window.clearInterval(timer)
  }, [load])

  const providers = useMemo(
    () =>
      (['openai', 'anthropic'] as const).map(
        (id) => snapshot.providers.find((p) => p.provider === id) ?? emptyProvider(id),
      ),
    [snapshot],
  )

  async function refreshNow() {
    setRefreshing(true)
    try {
      setSnapshot(await invoke<UsageSnapshot>('refresh_now'))
    } finally {
      setRefreshing(false)
    }
  }

  async function switchSurface(next: typeof surface) {
    setSurface(next)
    await invoke('set_surface', { surface: next })
  }

  async function patchSettings(patch: Partial<AppSettings>) {
    if (!settings) return
    const next = { ...settings, ...patch }
    const saved = await invoke<AppSettings>('save_settings', { settings: next })
    setSettings(saved)
  }

  async function finishOnboarding() {
    if (!settings) return
    const saved = await invoke<AppSettings>('save_settings', {
      settings: { ...settings, firstRunComplete: true },
    })
    setSettings(saved)
    await refreshNow()
    await switchSurface('compact')
  }

  if (!settings) return <div className="loading-shell" />

  return (
    <main className={`app-shell surface-${surface}`}>
      <div className="drag-rail" data-tauri-drag-region />
      {surface === 'compact' && (
        <CompactView
          providers={providers}
          showLongWindow={settings.showLongWindow}
          onOpen={() => void switchSurface('detail')}
        />
      )}
      {surface === 'detail' && (
        <DetailView
          providers={providers}
          settings={settings}
          refreshing={refreshing}
          onRefresh={() => void refreshNow()}
          onSettings={() => void switchSurface('settings')}
          onClose={() => void switchSurface('compact')}
        />
      )}
      {surface === 'settings' && (
        <SettingsView
          settings={settings}
          providers={providers}
          onChange={patchSettings}
          onBack={() => void switchSurface('detail')}
        />
      )}
      {surface === 'onboarding' && (
        <Onboarding
          providers={providers}
          refreshing={refreshing}
          onRefresh={() => void refreshNow()}
          onContinue={() => void finishOnboarding()}
        />
      )}
    </main>
  )
}

function CompactView({
  providers,
  showLongWindow,
  onOpen,
}: {
  providers: ProviderUsage[]
  showLongWindow: boolean
  onOpen: () => void
}) {
  return (
    <button className="compact-view" onClick={onOpen} aria-label="Open usage details">
      {providers.map((provider, index) => (
        <div className="compact-provider" key={provider.provider} title={statusLabel(provider)}>
          <span className="provider-logo">
            <ProviderLogo provider={provider.provider} size={19} />
          </span>
          <div className="compact-values">
            {compactWindows(provider, showLongWindow).map((window, windowIndex) => (
              <span
                key={`${provider.provider}-${window.id}`}
                className={`quota quota-${quotaTone(window.remainingPercent)}`}
              >
                {windowIndex > 0 && <span className="value-dot">·</span>}
                {percentage(window.remainingPercent)}
              </span>
            ))}
          </div>
          {provider.status === 'stale' && <span className="stale-dot" aria-label="Stale data" />}
          {index === 0 && <span className="provider-divider" />}
        </div>
      ))}
    </button>
  )
}

function DetailView({
  providers,
  settings,
  refreshing,
  onRefresh,
  onSettings,
  onClose,
}: {
  providers: ProviderUsage[]
  settings: AppSettings
  refreshing: boolean
  onRefresh: () => void
  onSettings: () => void
  onClose: () => void
}) {
  return (
    <section className="detail-view">
      <header className="surface-header">
        <span className="surface-title">Usage</span>
        <div className="surface-actions">
          <button
            className="icon-button"
            onClick={onRefresh}
            disabled={refreshing}
            title="Refresh now"
          >
            ↻
          </button>
          <button className="icon-button" onClick={onSettings} title="Settings">
            ⚙
          </button>
          <button className="icon-button" onClick={onClose} title="Compact mode">
            —
          </button>
        </div>
      </header>
      <div className="detail-providers">
        {providers.map((provider) => (
          <ProviderDetail key={provider.provider} provider={provider} settings={settings} />
        ))}
      </div>
      <footer className="detail-footer">
        <span>Last updated</span>
        <span className="tabular">
          {updatedTime(Math.max(...providers.map((p) => p.lastUpdated ?? 0)))}
        </span>
      </footer>
    </section>
  )
}

function ProviderDetail({
  provider,
  settings,
}: {
  provider: ProviderUsage
  settings: AppSettings
}) {
  const visible = provider.windows.slice(0, settings.showLongWindow ? 2 : 1)
  return (
    <article className="provider-detail">
      <div className="provider-heading">
        <ProviderLogo provider={provider.provider} size={21} />
        <span className={`status-chip status-${provider.status}`}>{statusLabel(provider)}</span>
      </div>
      {visible.length === 0 ? (
        <div className="provider-unavailable">
          <span>
            {provider.status === 'authentication_required'
              ? 'Authentication required'
              : 'Usage unavailable'}
          </span>
          {provider.lastUpdated && (
            <small>Last successful update: {updatedTime(provider.lastUpdated)}</small>
          )}
        </div>
      ) : (
        <div className="window-list">
          {visible.map((window) => (
            <div className="window-row" key={window.id}>
              <span>{window.label || 'Usage window'}</span>
              <span className={`tabular quota quota-${quotaTone(window.remainingPercent)}`}>
                {percentage(window.remainingPercent)}
              </span>
              {settings.showResetCountdown && (
                <>
                  <span className="reset-label">Reset</span>
                  <span className="tabular reset-value" title={resetTime(window.resetAt)}>
                    {countdown(window.resetAt)}
                  </span>
                </>
              )}
            </div>
          ))}
        </div>
      )}
    </article>
  )
}

function SettingsView({
  settings,
  providers,
  onChange,
  onBack,
}: {
  settings: AppSettings
  providers: ProviderUsage[]
  onChange: (patch: Partial<AppSettings>) => Promise<void>
  onBack: () => void
}) {
  return (
    <section className="settings-view">
      <header className="surface-header">
        <button className="back-button" onClick={onBack}>
          ←
        </button>
        <span className="surface-title">Settings</span>
      </header>
      <div className="settings-group">
        <SettingToggle
          label="Launch at startup"
          checked={settings.launchAtStartup}
          onChange={(v) => void onChange({ launchAtStartup: v })}
        />
        <SettingToggle
          label="Always on top"
          checked={settings.alwaysOnTop}
          onChange={(v) => void onChange({ alwaysOnTop: v })}
        />
        <label className="setting-row">
          <span>Refresh interval</span>
          <select
            value={settings.refreshIntervalMinutes}
            onChange={(e) => void onChange({ refreshIntervalMinutes: Number(e.target.value) })}
          >
            {[1, 3, 5, 10, 15].map((m) => (
              <option value={m} key={m}>
                {m} min
              </option>
            ))}
          </select>
        </label>
        <SettingToggle
          label="Notifications"
          checked={settings.notifications}
          onChange={(v) => void onChange({ notifications: v })}
        />
      </div>
      <div className="settings-group">
        <SettingToggle
          label="Compact mode on launch"
          checked={settings.compactMode}
          onChange={(v) => void onChange({ compactMode: v })}
        />
        <SettingToggle
          label="Show reset countdown"
          checked={settings.showResetCountdown}
          onChange={(v) => void onChange({ showResetCountdown: v })}
        />
        <SettingToggle
          label="Show weekly window"
          checked={settings.showLongWindow}
          onChange={(v) => void onChange({ showLongWindow: v })}
        />
      </div>
      <div className="provider-status-list">
        {providers.map((p) => (
          <div className="provider-status-row" key={p.provider}>
            <ProviderLogo provider={p.provider} size={16} />
            <span>{p.provider === 'openai' ? 'ChatGPT / Codex' : 'Claude'}</span>
            <span className="muted">{statusLabel(p)}</span>
          </div>
        ))}
      </div>
      <div className="about-row">
        <span>Usage</span>
        <span className="muted">v0.1.0 · MIT</span>
      </div>
    </section>
  )
}

function SettingToggle({
  label,
  checked,
  onChange,
}: {
  label: string
  checked: boolean
  onChange: (value: boolean) => void
}) {
  return (
    <label className="setting-row">
      <span>{label}</span>
      <input
        className="switch"
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
      />
    </label>
  )
}

function Onboarding({
  providers,
  refreshing,
  onRefresh,
  onContinue,
}: {
  providers: ProviderUsage[]
  refreshing: boolean
  onRefresh: () => void
  onContinue: () => void
}) {
  return (
    <section className="onboarding-view">
      <div className="onboarding-title">Usage</div>
      <p>Subscription usage, quietly available on your desktop.</p>
      <div className="detect-list">
        {providers.map((provider) => (
          <div className="detect-row" key={provider.provider}>
            <ProviderLogo provider={provider.provider} size={20} />
            <span>{provider.provider === 'openai' ? 'ChatGPT / Codex' : 'Claude'}</span>
            <span
              className={
                provider.status === 'fresh' || provider.status === 'stale' ? 'detected' : 'muted'
              }
            >
              {provider.status === 'fresh' || provider.status === 'stale'
                ? 'Detected'
                : provider.status === 'authentication_required'
                  ? 'Sign in required'
                  : 'Not detected'}
            </span>
          </div>
        ))}
      </div>
      <div className="onboarding-actions">
        <button className="secondary-button" onClick={onRefresh} disabled={refreshing}>
          {refreshing ? 'Checking…' : 'Check again'}
        </button>
        <button className="primary-button" onClick={onContinue}>
          Continue
        </button>
      </div>
    </section>
  )
}

function statusLabel(provider: ProviderUsage): string {
  switch (provider.status) {
    case 'fresh':
      return 'Available'
    case 'stale':
      return 'Stale'
    case 'authentication_required':
      return 'Sign in required'
    case 'rate_limited':
      return 'Rate limited'
    case 'unsupported':
      return 'Not detected'
    default:
      return 'Unavailable'
  }
}
