import type { GraphRow, StashInfo } from "./api";
import { ROW_HEIGHT } from "./virtual";

export interface StashPlacement {
  stash: StashInfo;
  rowIndex: number;
  stackIndex: number;
  stackCount: number;
  x: number;
  y: number;
}

export function indexStashes(stashes: StashInfo[]): ReadonlyMap<string, StashInfo[]> {
  const byBase = new Map<string, StashInfo[]>();
  for (const stash of stashes) {
    const group = byBase.get(stash.base_oid);
    if (group) group.push(stash);
    else byBase.set(stash.base_oid, [stash]);
  }
  return byBase;
}

export function placeVisibleStashes(
  byBase: ReadonlyMap<string, StashInfo[]>,
  rows: (GraphRow | undefined)[],
  start: number,
  scrollTop: number,
  viewportHeight: number,
  markerStartX = 0,
): StashPlacement[] {
  const placements: StashPlacement[] = [];
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index];
    if (!row) continue;
    const y = (start + index) * ROW_HEIGHT - scrollTop + ROW_HEIGHT / 2;
    if (y < 0 || y > viewportHeight) continue;
    const stashes = byBase.get(row.commit_id);
    if (!stashes) continue;
    stashes.forEach((stash, stackIndex) => {
      placements.push({
        stash,
        rowIndex: start + index,
        stackIndex,
        stackCount: stashes.length,
        x: markerStartX + stackIndex * 14,
        y,
      });
    });
  }
  return placements;
}

export function formatRelativeDate(timestamp: number, now = Date.now()): string {
  const seconds = Math.round((timestamp * 1000 - now) / 1000);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const units: [number, Intl.RelativeTimeFormatUnit][] = [
    [60, "second"],
    [60, "minute"],
    [24, "hour"],
    [7, "day"],
    [4.34524, "week"],
    [12, "month"],
    [Number.POSITIVE_INFINITY, "year"],
  ];
  let value = seconds;
  for (const [limit, unit] of units) {
    if (Math.abs(value) < limit) return formatter.format(Math.round(value), unit);
    value /= limit;
  }
  return formatter.format(Math.round(value), "year");
}
