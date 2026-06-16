# CEESVEE brand assets

The CEESVEE visual identity. The mark is a gradient "squircle" containing a
table with a solid header row, gridlines, a highlighted active cell, and the
signature fill-handle — a compact picture of a spreadsheet editor.

## Files

| File                                   | Use                                                                                                   |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| [`ceesvee-icon.svg`](ceesvee-icon.svg) | Master source. Regenerate every app icon from it with `npm run tauri icon branding/ceesvee-icon.svg`. |
| `../src-tauri/icons/*`                 | Generated app icons: `icon.ico` (Windows), `icon.icns` (macOS), PNGs, and Windows tile/store logos.   |
| `../public/favicon.svg`                | Simplified mark for the web favicon / browser tab.                                                    |
| `../src/components/Logo.tsx`           | The mark as a React component, used in the toolbar and empty state.                                   |

## Colors

| Token          | Hex                   | Use                              |
| -------------- | --------------------- | -------------------------------- |
| Violet         | `#7c3aed`             | Primary / gradient start, accent |
| Indigo         | `#4f46e5`             | Gradient end                     |
| Active cell    | `#ddd6fe`             | Highlight / selection accent     |
| Ink            | `#18181b`             | Text on light                    |
| Surface (dark) | `#18181b` – `#27272a` | Dark UI surfaces                 |

The UI accent is Tailwind `violet-600`; the gradient runs `violet-500 → indigo-500`.

## Typography

**Inter** for UI and the wordmark (`font-semibold` / `font-bold`, tight tracking),
with a monospace stack (`JetBrains Mono`, `ui-monospace`, …) available for data cells.

## Regenerating icons

```bash
npm run tauri icon branding/ceesvee-icon.svg
```

Edit `ceesvee-icon.svg` and re-run to propagate a design change to every platform.
