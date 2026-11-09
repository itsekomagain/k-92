// Small line-icon set (24px grid, 1.75 stroke) so the app has no icon-font dependency.

const paths = {
  overview: "M3 12l9-8 9 8M5 10v10h5v-6h4v6h5V10",
  timeline: "M12 7v5l3 2M3.5 12a8.5 8.5 0 1 0 2.5-6M3 4v4h4",
  storage: "M4 6.5C4 5 7.6 4 12 4s8 1 8 2.5S16.4 9 12 9 4 8 4 6.5zM4 6.5v11C4 19 7.6 20 12 20s8-1 8-2.5v-11M4 12c0 1.5 3.6 2.5 8 2.5s8-1 8-2.5",
  restore: "M12 4v10m0 0l-4-4m4 4l4-4M5 16v3h14v-3",
  activity: "M3 12h4l3-7 4 14 3-7h4",
  settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 13.5l1.6 1.2-2 3.4-1.9-.6a7.6 7.6 0 0 1-2.1 1.2L14.6 21h-4l-.4-2.3a7.6 7.6 0 0 1-2.1-1.2l-1.9.6-2-3.4 1.6-1.2a7.7 7.7 0 0 1 0-3l-1.6-1.2 2-3.4 1.9.6a7.6 7.6 0 0 1 2.1-1.2L10.6 3h4l.4 2.3a7.6 7.6 0 0 1 2.1 1.2l1.9-.6 2 3.4-1.6 1.2a7.7 7.7 0 0 1 0 3z",
  shield: "M12 3l8 3v6c0 4.5-3.4 8.3-8 9-4.6-.7-8-4.5-8-9V6l8-3zm-3.5 9l2.5 2.5 4.5-5",
  shieldAlert: "M12 3l8 3v6c0 4.5-3.4 8.3-8 9-4.6-.7-8-4.5-8-9V6l8-3zm0 5v5m0 3v.01",
  folder: "M3 7.5A1.5 1.5 0 0 1 4.5 6h4.6l2 2h8.4A1.5 1.5 0 0 1 21 9.5v8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z",
  file: "M7 3h7l5 5v12a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm7 0v5h5",
  drive: "M3 14l2.5-8h13L21 14M3 14v4a1 1 0 0 0 1 1h16a1 1 0 0 0 1-1v-4M3 14h18M17 16.5h.01",
  plus: "M12 5v14M5 12h14",
  x: "M6 6l12 12M18 6L6 18",
  check: "M5 12.5l4.5 4.5L19 7.5",
  search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zm9 2l-4-4",
  play: "M8 5v14l11-7z",
  pause: "M8 5v14M16 5v14",
  chevron: "M9 6l6 6-6 6",
  alert: "M12 4l9 16H3zM12 10v4m0 3v.01",
  info: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zm0-10v6m0-9v.01",
  refresh: "M20 11a8 8 0 0 0-14.9-3M4 4v4h4M4 13a8 8 0 0 0 14.9 3M20 20v-4h-4",
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name, size = 20, className }: { name: IconName; size?: number; className?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={className}
    >
      <path d={paths[name]} />
    </svg>
  );
}
