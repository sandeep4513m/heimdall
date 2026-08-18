<!--
  Svelte 5 native icon component — replaces @tabler/icons-svelte
  which uses Svelte 4 patterns ($$props, <slot/>, <svelte:element>)
  that crash in Svelte 5.55+ with the next_sibling_getter error.
-->
<script lang="ts">
  interface Props {
    size?: number;
    stroke?: number;
    color?: string;
    class?: string;
    paths: string[];
  }

  let { size = 24, stroke = 2, color = 'currentColor', paths, class: className = '' }: Props = $props();
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke={color}
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  class={className}
>
  {#each paths as d}
    <path {d} />
  {/each}
</svg>
