<script lang="ts">
  import { onMount } from "svelte";
  import type { GraphRow } from "./lib/api";
  import type { StashPlacement } from "./lib/stashes";
  import { ROW_HEIGHT } from "./lib/virtual";

  let {
    rows,
    start,
    scrollTop,
    height,
    selected,
    width = 168,
    laneWidth = 168,
    laneViewportWidth = width,
    visible = true,
    stashPlacements = [],
    onSelectStash,
    onContextStash,
  }: {
    rows: (GraphRow | undefined)[]; start: number; scrollTop: number;
    height: number; selected: string | null; width?: number; laneWidth?: number;
    laneViewportWidth?: number; visible?: boolean;
    stashPlacements?: StashPlacement[];
    onSelectStash?: (oid: string) => void;
    onContextStash?: (event: MouseEvent, oid: string) => void;
  } = $props();
  let canvas: HTMLCanvasElement;
  let dpr = $state(1);
  const colors = ["#54d4b0", "#ac96fa", "#65b9f4", "#e7b665", "#f080a0", "#65d2de"];

  onMount(() => {
    let query: MediaQueryList;
    const update = () => {
      dpr = window.devicePixelRatio || 1;
      query?.removeEventListener("change", update);
      query = window.matchMedia(`(resolution: ${dpr}dppx)`);
      query.addEventListener("change", update, { once: true });
    };
    update();
    return () => query?.removeEventListener("change", update);
  });

  $effect(() => {
    if (!canvas || !visible) return;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    const context = canvas.getContext("2d");
    if (!context) return;
    context.scale(dpr, dpr);
    context.lineWidth = 2;
    const x = (lane: number) => 16 + lane * 15 * Math.min(1, (laneViewportWidth - 32) / (laneWidth - 32));
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i];
      if (!row) continue;
      const y = (start + i) * ROW_HEIGHT - scrollTop + ROW_HEIGHT / 2;
      if (y < -ROW_HEIGHT || y > height + ROW_HEIGHT) continue;
      for (const edge of row.parent_edges) {
        context.strokeStyle = colors[edge.to_lane % colors.length];
        context.beginPath();
        context.moveTo(x(edge.from_lane), y);
        context.bezierCurveTo(x(edge.from_lane), y + 18, x(edge.to_lane), y + 18, x(edge.to_lane), y + ROW_HEIGHT);
        context.stroke();
      }
    }
    context.setLineDash([3, 3]);
    for (const placement of stashPlacements) {
      const row = rows[placement.rowIndex - start];
      if (!row) continue;
      const y = placement.y;
      context.strokeStyle = "#b69df0";
      context.beginPath();
      context.moveTo(x(row.lane), y);
      context.lineTo(placement.x, y);
      context.stroke();
    }
    context.setLineDash([]);
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i];
      if (!row) continue;
      const y = (start + i) * ROW_HEIGHT - scrollTop + ROW_HEIGHT / 2;
      if (y < -8 || y > height + 8) continue;
      context.beginPath();
      context.arc(x(row.lane), y, row.commit_id === selected ? 5 : 4, 0, Math.PI * 2);
      context.fillStyle = row.commit_id === selected ? "#f5f8ff" : colors[row.lane % colors.length];
      context.fill();
      context.strokeStyle = "#1a1e28";
      context.stroke();
    }
    for (const placement of stashPlacements) {
      context.fillStyle = "#302a42";
      context.fillRect(placement.x - 5, placement.y - 5, 10, 10);
      context.strokeStyle = "#b69df0";
      context.strokeRect(placement.x - 5, placement.y - 5, 10, 10);
    }
  });
</script>

<canvas bind:this={canvas} class="graph-canvas" style:width="{width}px" style:height="{height}px" aria-hidden="true"></canvas>
{#if visible}
  {#each stashPlacements as placement (placement.stash.oid)}
    <button
      class="stash-marker"
      class:selected={selected === placement.stash.oid}
      style:left="{placement.x - 7}px"
      style:top="{placement.y - 7}px"
      aria-label={`Select stash ${placement.stash.message}`}
      title={placement.stash.message}
      onclick={(event) => { event.stopPropagation(); onSelectStash?.(placement.stash.oid); }}
      oncontextmenu={(event) => { event.preventDefault(); event.stopPropagation(); onContextStash?.(event, placement.stash.oid); }}
    ></button>
  {/each}
{/if}
