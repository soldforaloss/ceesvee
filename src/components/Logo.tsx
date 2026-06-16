// The CEESVEE mark — the same artwork as the app icon (branding/ceesvee-icon.svg),
// as a scalable React component. Size it with `className` (e.g. "h-16 w-16").

export function Logo({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 1024 1024"
      className={className}
      role="img"
      aria-label="CEESVEE"
      xmlns="http://www.w3.org/2000/svg"
    >
      <defs>
        <linearGradient
          id="cv-tile"
          x1="96"
          y1="96"
          x2="928"
          y2="928"
          gradientUnits="userSpaceOnUse"
        >
          <stop offset="0" stopColor="#7c3aed" />
          <stop offset="1" stopColor="#4f46e5" />
        </linearGradient>
        <radialGradient id="cv-sheen" cx="330" cy="260" r="660" gradientUnits="userSpaceOnUse">
          <stop offset="0" stopColor="#ffffff" stopOpacity="0.22" />
          <stop offset="0.62" stopColor="#ffffff" stopOpacity="0" />
        </radialGradient>
        <clipPath id="cv-grid">
          <rect x="242" y="242" width="540" height="540" rx="40" />
        </clipPath>
      </defs>

      <rect x="64" y="64" width="896" height="896" rx="208" fill="url(#cv-tile)" />
      <rect x="64" y="64" width="896" height="896" rx="208" fill="url(#cv-sheen)" />

      <g clipPath="url(#cv-grid)">
        <rect x="242" y="242" width="540" height="180" fill="#ffffff" />
        <rect x="422" y="422" width="180" height="180" fill="#ddd6fe" />
        <g stroke="#ffffff" strokeWidth="16">
          <line x1="422" y1="242" x2="422" y2="782" />
          <line x1="602" y1="242" x2="602" y2="782" />
          <line x1="242" y1="422" x2="782" y2="422" />
          <line x1="242" y1="602" x2="782" y2="602" />
        </g>
      </g>

      <rect
        x="242"
        y="242"
        width="540"
        height="540"
        rx="40"
        fill="none"
        stroke="#ffffff"
        strokeWidth="18"
      />
      <rect
        x="584"
        y="584"
        width="36"
        height="36"
        rx="7"
        fill="#ffffff"
        stroke="#4f46e5"
        strokeWidth="8"
      />
    </svg>
  );
}
