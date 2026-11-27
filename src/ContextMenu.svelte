<script lang="ts">
  import { onMount } from "svelte";
  import type { MenuItem } from "./lib/menus";

  let { x, y, items, onSelect, onClose }: {
    x: number;
    y: number;
    items: MenuItem[];
    onSelect: (id: string) => void;
    onClose: () => void;
  } = $props();
  let menu: HTMLDivElement;
  let buttons: HTMLButtonElement[] = [];
  let left = $state(0);
  let top = $state(0);

  function position() {
    if (!menu) return;
    left = Math.max(8, Math.min(x, window.innerWidth - menu.offsetWidth - 8));
    top = Math.max(8, Math.min(y, window.innerHeight - menu.offsetHeight - 8));
  }

  function enabledIndices() {
    return items.flatMap((item, index) => item.disabled ? [] : [index]);
  }

  function moveFocus(direction: 1 | -1) {
    const enabled = enabledIndices();
    if (!enabled.length) return;
    const current = enabled.indexOf(document.activeElement === document.body
      ? -1
      : buttons.findIndex(button => button === document.activeElement));
    const next = current < 0
      ? enabled[direction > 0 ? 0 : enabled.length - 1]
      : enabled[(current + direction + enabled.length) % enabled.length];
    buttons[next]?.focus();
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveFocus(event.key === "ArrowDown" ? 1 : -1);
    }
  }

  function handlePointerDown(event: PointerEvent) {
    if (menu && !menu.contains(event.target as Node)) onClose();
  }

  $effect(() => {
    position();
  });

  onMount(() => {
    position();
    buttons.find(button => button && !button.disabled)?.focus();
    const closeOnScroll = () => onClose();
    window.addEventListener("resize", position);
    document.addEventListener("scroll", closeOnScroll, true);
    return () => {
      window.removeEventListener("resize", position);
      document.removeEventListener("scroll", closeOnScroll, true);
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} onpointerdown={handlePointerDown} onblur={onClose} />

<div bind:this={menu} class="context-menu" role="menu" style:left="{left}px" style:top="{top}px">
  {#each items as item, index (item.id)}
    <button
      bind:this={buttons[index]}
      role="menuitem"
      class:danger={item.danger}
      disabled={item.disabled}
      onclick={() => { onSelect(item.id); }}
    >{item.label}</button>
  {/each}
</div>
