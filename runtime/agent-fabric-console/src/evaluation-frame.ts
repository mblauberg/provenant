import stringWidth from "string-width";
import { splitGraphemes } from "unicode-segmenter/grapheme";

import type { FabricConsoleFrame } from "./index.js";

function cellSlice(value: string, start: number, end: number): string {
  let column = 0;
  let output = "";
  for (const grapheme of splitGraphemes(value)) {
    const nextColumn = column + stringWidth(grapheme);
    if (nextColumn > start && column < end) output += grapheme;
    column = nextColumn;
    if (column >= end) break;
  }
  return output;
}

export function frameHasEnabledVisibleFocus(
  frame: FabricConsoleFrame,
): boolean {
  if (frame.mode === "inert") return false;
  const focusId = frame.presentation.focusId;
  if (focusId === null) return false;
  const region = frame.hitRegions.find(
    ({ enabled, id }) => enabled && id === focusId,
  );
  if (region === undefined) return false;
  const firstRow = frame.rows[region.rect.y1 - 1];
  return firstRow !== undefined &&
    cellSlice(firstRow, region.rect.x1 - 1, region.rect.x1) === ">";
}
