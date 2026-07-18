// Glide Data Grid theme objects for light and dark modes, plus the per-cell
// overrides used to decorate unsaved (dirty) cells and F42 highlight matches.

import type { Theme } from "@glideapps/glide-data-grid";

import { highlightBackground, highlightFontStyle } from "./highlight";
import type { HighlightDecoration } from "../types";

const shared: Partial<Theme> = {
  fontFamily: "Inter, ui-sans-serif, system-ui, sans-serif",
  baseFontStyle: "13px",
  headerFontStyle: "600 13px",
  editorFontSize: "13px",
  cellHorizontalPadding: 8,
  cellVerticalPadding: 3,
  roundingRadius: 3,
};

export const lightGridTheme: Partial<Theme> = {
  ...shared,
  accentColor: "#6d28d9",
  accentFg: "#ffffff",
  accentLight: "rgba(109, 40, 217, 0.10)",
  textDark: "#18181b",
  textMedium: "#52525b",
  textLight: "#a1a1aa",
  textHeader: "#3f3f46",
  bgCell: "#ffffff",
  bgCellMedium: "#fafafa",
  bgHeader: "#f4f4f5",
  bgHeaderHasFocus: "#e4e4e7",
  bgHeaderHovered: "#ececee",
  borderColor: "rgba(0, 0, 0, 0.08)",
  horizontalBorderColor: "rgba(0, 0, 0, 0.06)",
  drilldownBorder: "rgba(0, 0, 0, 0.2)",
  linkColor: "#6d28d9",
};

export const darkGridTheme: Partial<Theme> = {
  ...shared,
  accentColor: "#8b5cf6",
  accentFg: "#ffffff",
  accentLight: "rgba(139, 92, 246, 0.20)",
  textDark: "#f4f4f5",
  textMedium: "#a1a1aa",
  textLight: "#71717a",
  textHeader: "#d4d4d8",
  bgCell: "#18181b",
  bgCellMedium: "#1f1f23",
  bgHeader: "#27272a",
  bgHeaderHasFocus: "#3f3f46",
  bgHeaderHovered: "#323237",
  bgBubble: "#27272a",
  borderColor: "rgba(255, 255, 255, 0.08)",
  horizontalBorderColor: "rgba(255, 255, 255, 0.06)",
  drilldownBorder: "rgba(255, 255, 255, 0.2)",
  linkColor: "#a78bfa",
};

/** Per-cell override applied to unsaved cells (works on both themes). */
export const dirtyCellOverride: Partial<Theme> = {
  bgCell: "rgba(139, 92, 246, 0.14)",
};

/**
 * The glide theme override that paints one F42 highlight decoration: a
 * translucent tone tint (so the cell's own text keeps its theme contrast) plus
 * an optional bold/italic weight. Theme-aware — the same rule reads correctly
 * in light and dark. The icon is carried separately (shown in the rule list
 * and explain popover), so it is not part of the canvas override.
 */
export function highlightCellOverride(
  decoration: HighlightDecoration,
  dark: boolean,
): Partial<Theme> {
  const override: Partial<Theme> = {
    bgCell: highlightBackground(decoration.tone, decoration.emphasis, dark),
  };
  const font = highlightFontStyle(decoration.textStyle);
  if (font) override.baseFontStyle = font;
  return override;
}
