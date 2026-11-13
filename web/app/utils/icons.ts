// Inline stroke icon paths (24×24 grid), same style as the approved mockups.
export const ICONS = {
  dashboard: 'M3 3h7v9H3zM14 3h7v5h-7zM14 12h7v9h-7zM3 16h7v5H3z',
  sites: 'M12 3a9 9 0 100 18 9 9 0 000-18zM3 12h18M12 3c2.5 2.7 3.8 5.7 3.8 9s-1.3 6.3-3.8 9c-2.5-2.7-3.8-5.7-3.8-9S9.5 5.7 12 3z',
  php: 'M9 9h6v6H9zM5 5h14v14H5zM9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3',
  services: 'M4 4h16v6H4zM4 14h16v6H4zM8 7h.01M8 17h.01',
  databases: 'M12 3c4.4 0 8 1.3 8 3s-3.6 3-8 3-8-1.3-8-3 3.6-3 8-3zM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3',
  security: 'M12 3l8 3v6c0 4.6-3.4 8.1-8 9-4.6-.9-8-4.4-8-9V6l8-3zM9 12l2 2 4-4',
  jobs: 'M4 17l6-6-6-6M12 19h8',
  logout: 'M15 3h4a2 2 0 012 2v14a2 2 0 01-2 2h-4M10 17l5-5-5-5M15 12H3',
  restart: 'M21 12a9 9 0 11-3-6.7L21 8M21 3v5h-5',
  search: 'M11 4a7 7 0 100 14 7 7 0 000-14zM20 20l-3.5-3.5',
  alert: 'M12 3l10 18H2L12 3zM12 10v5M12 18v.01',
  check: 'M20 6L9 17l-5-5',
  info: 'M12 3a9 9 0 100 18 9 9 0 000-18zM12 11v6M12 7.5v.01',
  chevron: 'M9 6l6 6-6 6',
  download: 'M12 4v11M7 10l5 5 5-5M5 20h14',
  backup: 'M3 12a9 9 0 109-9 9.7 9.7 0 00-6.7 2.7L3 8M3 3v5h5M12 7v5l3 2',
  git: 'M6 3v12M18 9a3 3 0 100-6 3 3 0 000 6zM6 21a3 3 0 100-6 3 3 0 000 6zM18 9a9 9 0 01-9 9',
  lock: 'M6 11h12v10H6zM8 11V7a4 4 0 018 0v4',
  close: 'M6 6l12 12M18 6L6 18',
} as const

export type IconName = keyof typeof ICONS
