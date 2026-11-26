export const ROW_HEIGHT = 36;
export const PAGE_SIZE = 200;
export const OVERSCAN = 8;
export const MAX_PAGES = 12;

export function visibleRange(scrollTop: number, viewportHeight: number, total: number, overscan = OVERSCAN) {
  const start = Math.min(total, Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - overscan));
  const end = Math.min(total, Math.max(start, Math.ceil((Math.max(0, scrollTop) + viewportHeight) / ROW_HEIGHT) + overscan));
  return { start, end };
}

export function pageOffsets(start: number, end: number, pageSize = PAGE_SIZE): number[] {
  if (end <= start) return [];
  const offsets: number[] = [];
  for (let offset = Math.floor(start / pageSize) * pageSize; offset < end; offset += pageSize) offsets.push(offset);
  return offsets;
}

/** Insertion-ordered LRU; viewport reads refresh pages before new pages arrive. */
export class PageCache<T> {
  private readonly pages = new Map<number, readonly T[]>();
  constructor(readonly capacity = MAX_PAGES, readonly pageSize = PAGE_SIZE) {
    if (capacity < 1 || pageSize < 1) throw new Error("Cache capacity and page size must be positive.");
  }
  get size() { return this.pages.size; }
  has(offset: number) { return this.pages.has(offset); }
  get(offset: number) {
    const page = this.pages.get(offset);
    if (page) { this.pages.delete(offset); this.pages.set(offset, page); }
    return page;
  }
  row(index: number) {
    return this.get(Math.floor(index / this.pageSize) * this.pageSize)?.[index % this.pageSize];
  }
  set(offset: number, rows: readonly T[]) {
    this.pages.delete(offset);
    this.pages.set(offset, rows);
    while (this.pages.size > this.capacity) this.pages.delete(this.pages.keys().next().value!);
  }
  clear() { this.pages.clear(); }
}
