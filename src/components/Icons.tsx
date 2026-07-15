// Minimal inline icon set (Feather-style, 24×24, stroke = currentColor) so we
// don't pull in an icon dependency.

type IconProps = { className?: string };

const base = {
  width: 18,
  height: 18,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

export const FilePlus = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <path d="M14 2v6h6M12 12v6M9 15h6" />
  </svg>
);

export const FolderOpen = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h6a2 2 0 0 1 2 2v1H3z" />
    <path d="M3 10h18l-2 8a2 2 0 0 1-2 1.5H5a2 2 0 0 1-2-1.5z" />
  </svg>
);

export const Save = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z" />
    <path d="M17 21v-8H7v8M7 3v5h8" />
  </svg>
);

export const Undo = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M3 7v6h6" />
    <path d="M3 13a9 9 0 1 0 3-7L3 9" />
  </svg>
);

export const Redo = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M21 7v6h-6" />
    <path d="M21 13a9 9 0 1 1-3-7l3 3" />
  </svg>
);

export const Search = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <circle cx="11" cy="11" r="7" />
    <path d="m21 21-4.3-4.3" />
  </svg>
);

export const Sun = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </svg>
);

export const Moon = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
  </svg>
);

export const RowPlus = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <rect x="3" y="4" width="18" height="5" rx="1" />
    <rect x="3" y="13" width="18" height="5" rx="1" />
    <path d="M12 20v3M10.5 21.5h3" />
  </svg>
);

export const Trash = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M3 6h18M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
  </svg>
);

export const ColumnPlus = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <rect x="4" y="3" width="5" height="18" rx="1" />
    <rect x="13" y="3" width="5" height="18" rx="1" />
    <path d="M21 9v6M18 12h6" transform="translate(0 0)" />
  </svg>
);

export const SortIcon = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M7 4v16M3 8l4-4 4 4M17 20V4M21 16l-4 4-4-4" />
  </svg>
);

export const Download = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3" />
  </svg>
);

export const Filter = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />
  </svg>
);

export const Refresh = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M23 4v6h-6M1 20v-6h6" />
    <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
  </svg>
);

export const Stats = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <line x1="18" y1="20" x2="18" y2="10" />
    <line x1="12" y1="20" x2="12" y2="4" />
    <line x1="6" y1="20" x2="6" y2="14" />
  </svg>
);

export const Close = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M18 6 6 18M6 6l12 12" />
  </svg>
);

export const ChevronUp = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="m18 15-6-6-6 6" />
  </svg>
);

export const ChevronDown = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="m6 9 6 6 6-6" />
  </svg>
);

export const Dots = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <circle cx="5" cy="12" r="1" />
    <circle cx="12" cy="12" r="1" />
    <circle cx="19" cy="12" r="1" />
  </svg>
);

export const Dot = ({ className }: IconProps) => (
  <svg width="8" height="8" viewBox="0 0 8 8" className={className}>
    <circle cx="4" cy="4" r="4" fill="currentColor" />
  </svg>
);

export const Pulse = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
  </svg>
);

export const Bookmark = ({ className }: IconProps) => (
  <svg {...base} className={className}>
    <path d="M19 21l-7-5-7 5V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" />
  </svg>
);
