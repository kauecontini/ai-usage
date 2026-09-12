import { describe, expect, it } from 'vitest'
import { countdown, percentage, quotaTone } from './format'

describe('format helpers', () => {
  it('keeps percentage display bounded and stable', () => {
    expect(percentage(9.6)).toBe('10%')
    expect(percentage(120)).toBe('100%')
    expect(percentage(-1)).toBe('0%')
    expect(percentage(null)).toBe('—')
  })

  it('formats reset countdowns', () => {
    expect(countdown(10_000, 9_000)).toBe('00h 16m')
    expect(countdown(20_000, 9_000)).toBe('03h 03m')
  })

  it('maps quota tones from remaining percentage', () => {
    expect(quotaTone(72)).toBe('neutral')
    expect(quotaTone(20)).toBe('warning')
    expect(quotaTone(8)).toBe('danger')
    expect(quotaTone(0)).toBe('exhausted')
  })
})
