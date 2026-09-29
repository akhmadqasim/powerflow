import type { StatusBarItem, Theme } from '@/bindings'
import { defineStore } from 'pinia'
import { ref } from 'vue'

export const usePreference = defineStore('preference', () => {
  const theme = ref<Theme>('system')
  const animationsEnabled = ref(true)
  const updateInterval = ref(1500)
  const language = ref('en')
  const statusBarItem = ref<StatusBarItem>('system')
  const statusBarShowCharging = ref(true)

  return {
    theme,
    animationsEnabled,
    updateInterval,
    language,
    statusBarItem,
    statusBarShowCharging,
  }
}, {
  tauri: {
    saveOnChange: true,
    saveStrategy: 'debounce',
    saveInterval: 1000,
  },
})

const VALID_STATUS_BAR_ITEMS: StatusBarItem[] = ['system', 'screen', 'heatpipe']
const MIN_INTERVAL = 500
const MAX_INTERVAL = 60_000

export function usePreferenceAsync() {
  const preference = usePreference()
  const isLoading = ref(true)
  preference.$tauri.start().then(() => {
    // Sanitize values persisted by older or buggy builds.
    if (!VALID_STATUS_BAR_ITEMS.includes(preference.statusBarItem))
      preference.statusBarItem = 'system'
    if (!(preference.updateInterval >= MIN_INTERVAL && preference.updateInterval <= MAX_INTERVAL))
      preference.updateInterval = 2000
    isLoading.value = false
  })
  return { preference, isLoading }
}
