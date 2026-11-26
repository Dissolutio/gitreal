<script lang="ts" generics="T">
  import { onMount, type Snippet } from "svelte";
  import { visibleRange } from "./lib/virtual";

  let { items, children, maxHeight = 216 }: {
    items: T[]; children: Snippet<[T, number]>; maxHeight?: number;
  } = $props();
  let element: HTMLDivElement;
  let top = $state(0);
  let height = $state(216);
  const range = $derived(visibleRange(top, height, items.length, 3));

  $effect(() => {
    const count = items.length;
    if (element && element.scrollTop > Math.max(0, count * 36 - height)) {
      element.scrollTop = Math.max(0, count * 36 - height);
      top = element.scrollTop;
    }
  });
  onMount(() => {
    const observer = new ResizeObserver(() => { height = element.clientHeight; });
    observer.observe(element);
    return () => observer.disconnect();
  });
</script>

<div class="virtual-list" bind:this={element} style:max-height="{maxHeight}px"
  onscroll={() => { top = element.scrollTop; }}>
  <div style:height="{items.length * 36}px" style:position="relative">
    {#each items.slice(range.start, range.end) as item, offset (range.start + offset)}
      <div class="virtual-item" style:top="{(range.start + offset) * 36}px">
        {@render children(item, range.start + offset)}
      </div>
    {/each}
  </div>
</div>
