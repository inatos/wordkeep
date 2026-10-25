<script lang="ts">
  let { name, size = '1rem' }: { name: string; size?: string } = $props();
</script>

<svg viewBox="0 0 24 24" style:width={size} style:height={size} aria-hidden="true">
  {#if name === 'plus'}
    <path d="M12 5v14M5 12h14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'chat'}
    <path d="M5 6h14v9H8l-3 3V6z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'search'}
    <circle cx="11" cy="11" r="6" fill="none" stroke="currentColor" stroke-width="2" /><path d="M16 16l4 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'speak'}
    <path d="M4 10v4h3l4 3V7L7 10H4zM16 9a4 4 0 0 1 0 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'fork'}
    <circle cx="6" cy="6" r="2" fill="none" stroke="currentColor" stroke-width="2" /><circle cx="6" cy="18" r="2" fill="none" stroke="currentColor" stroke-width="2" /><circle cx="18" cy="12" r="2" fill="none" stroke="currentColor" stroke-width="2" /><path d="M6 8v8M8 12h8" fill="none" stroke="currentColor" stroke-width="2" />
  {:else if name === 'regen'}
    <path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'copy'}
    <rect x="8" y="8" width="11" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="2" /><path d="M5 15V6a2 2 0 0 1 2-2h9" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'check'}
    <path d="M4 12l5 5L20 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'pin'}
    <path d="M8 4h8l-1 6 3 3v2H6v-2l3-3-1-6zM12 15v5" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'vault'}
    <path d="M4 6h16v12H4zM8 10h8M8 14h5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'workshop'}
    <path d="M4 18l6-6 3 3 7-7M14 8h6v6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'command'}
    <path d="M8 8h8v8H8zM4 9h4M16 9h4M4 15h4M16 15h4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'send'}
    <path d="M4 12l16-7-6 16-2-7-8-2z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'dream'}
    <path d="M16 4a7 7 0 1 0 4 12.5A6 6 0 1 1 16 4z" fill="none" stroke="currentColor" stroke-width="2" />
  {:else if name === 'distill'}
    <path d="M8 3h8M9 3v5L5 16a3 3 0 0 0 3 4h8a3 3 0 0 0 3-4l-4-8V3" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'branch'}
    <circle cx="6" cy="6" r="2" fill="none" stroke="currentColor" stroke-width="2" /><circle cx="6" cy="18" r="2" fill="none" stroke="currentColor" stroke-width="2" /><path d="M6 8v8" fill="none" stroke="currentColor" stroke-width="2" />
  {:else if name === 'bold'}
    <path d="M7 5h6a3 3 0 0 1 0 6H7zM7 11h7a3 3 0 0 1 0 6H7z" fill="none" stroke="currentColor" stroke-width="2" />
  {:else if name === 'italic'}
    <path d="M15 5H9M15 19H9M14 5l-4 14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'code'}
    <path d="M9 8L5 12l4 4M15 8l4 4-4 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'heading'}
    <path d="M5 6v12M12 6v12M5 12h7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'list'}
    <path d="M9 7h11M9 12h11M9 17h11M5 7h.01M5 12h.01M5 17h.01" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'quote'}
    <path d="M8 8H5v5h3V8zm8 0h-3v5h3V8zM8 13c0 2-1 3-3 4m11-4c0 2-1 3-3 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'link'}
    <path d="M10 14l4-4M8 12l-2 2a3 3 0 0 0 4 4l2-2M16 12l2-2a3 3 0 0 0-4-4l-2 2" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'eye'}
    <path d="M2 12s4-6 10-6 10 6 10 6-4 6-10 6S2 12 2 12z" fill="none" stroke="currentColor" stroke-width="2" /><circle cx="12" cy="12" r="2.5" fill="none" stroke="currentColor" stroke-width="2" />
  {:else if name === 'pencil'}
    <path d="M4 20l4.5-1L19 8.5 15.5 5 5 15.5 4 20zM13.5 6.5l4 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'trash'}
    <path d="M5 7h14M9 7V5h6v2M8 7l1 12h6l1-12" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'user'}
    <circle cx="12" cy="8" r="3" fill="none" stroke="currentColor" stroke-width="2" /><path d="M5 19c1.5-3 3.5-4 7-4s5.5 1 7 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'spark'}
    <path d="M12 3l1.5 5.5L19 10l-5.5 1.5L12 17l-1.5-5.5L5 10l5.5-1.5L12 3z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {:else if name === 'close'}
    <path d="M6 6l12 12M18 6L6 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'gpu'}
    <rect x="3" y="7" width="18" height="10" rx="2" fill="none" stroke="currentColor" stroke-width="2" /><path d="M7 11h4M7 14h2M14 11h3M14 14h3" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'pulse'}
    <path d="M3 12h4l2-5 4 10 2-5h6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'memory'}
    <path d="M7 4h10v16H7zM10 8h4M10 12h4M10 16h4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'clock'}
    <circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="2" /><path d="M12 8v5l3 2" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
  {:else if name === 'layers'}
    <path d="M12 4l8 4-8 4-8-4 8-4zM4 12l8 4 8-4M4 16l8 4 8-4" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" />
  {/if}
</svg>
