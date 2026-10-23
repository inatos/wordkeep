<script lang="ts">
  import { onMount } from 'svelte';
  import ElucifiaSelect from './ElucifiaSelect.svelte';
  import {
    CONDUIT_PALETTE,
    addNode,
    breakPinLinks,
    breakWire,
    conduitPreset,
    connectPins,
    duplicateNode,
    fitBounds,
    makeNode,
    paletteFor,
    pinIn,
    pinOut,
    removeNode,
    wirePath,
    type GraphCable,
    type GraphDoc,
    type GraphNode,
  } from './blueprint-graph';

  let {
    models = [],
    jobs = [],
    saved = [],
    onrun,
    onpromote,
    onpersist,
  }: {
    models?: string[];
    jobs?: string[];
    saved?: GraphDoc[];
    onrun?: (job: string, prompt: string) => void;
    onpromote?: (title: string, body: string) => void;
    onpersist?: (body: Record<string, unknown>) => void;
  } = $props();

  let doc = $state<GraphDoc>(conduitPreset('Concept Scout'));
  let selected = $state('prompt');
  let zoom = $state(1);
  let panX = $state(24);
  let panY = $state(18);
  let paletteOpen = $state(true);
  let inspectorOpen = $state(true);
  let drag = $state<'none' | 'node' | 'wire' | 'pan'>('none');
  let dragId = $state('');
  let dragDx = $state(0);
  let dragDy = $state(0);
  let wireFrom = $state<{ id: string; port: number; out: boolean; x: number; y: number } | null>(null);
  let cursor = $state({ x: 0, y: 0 });
  let menu = $state<{ x: number; y: number; wx: number; wy: number; query: string } | null>(null);
  let saveName = $state('');
  let surface: HTMLDivElement | null = $state(null);

  const current = $derived(doc.nodes.find((node) => node.id === selected) ?? null);
  const menuItems = $derived(
    CONDUIT_PALETTE.filter((item) => item.label.toLowerCase().includes((menu?.query || '').toLowerCase())),
  );

  onMount(() => {
    const run = () => {
      if (!surface || surface.clientWidth < 80 || surface.clientHeight < 80) return;
      fit();
    };
    const obs = new ResizeObserver(run);
    if (surface) obs.observe(surface);
    run();
    return () => obs.disconnect();
  });

  function worldAt(clientX: number, clientY: number) {
    const rect = surface?.getBoundingClientRect();
    if (!rect) return { x: 80, y: 80 };
    return { x: (clientX - rect.left - panX) / zoom, y: (clientY - rect.top - panY) / zoom };
  }

  function spawn(kind: string, x: number, y: number, wire: typeof wireFrom = null) {
    const node = makeNode(kind, x, y);
    let cables = doc.cables;
    if (wire?.out) cables = connectPins(cables, wire.id, node.id, wire.port, 0);
    else if (wire && !wire.out) cables = connectPins(cables, node.id, wire.id, 0, wire.port);
    doc = { ...doc, nodes: addNode(doc.nodes, node), cables };
    selected = node.id;
    menu = null;
    wireFrom = null;
  }

  function loadNamed(name: string) {
    doc = conduitPreset(name);
    selected = doc.nodes[0]?.id || '';
    fit();
  }

  function fit() {
    const rect = surface?.getBoundingClientRect();
    const next = fitBounds(doc.nodes, rect?.width || 900, rect?.height || 560);
    zoom = next.zoom;
    panX = next.panX;
    panY = next.panY;
  }

  function onWheel(event: WheelEvent) {
    event.preventDefault();
    const before = worldAt(event.clientX, event.clientY);
    const next = Math.max(0.25, Math.min(4, zoom * (event.deltaY < 0 ? 1.08 : 0.92)));
    zoom = next;
    const rect = surface?.getBoundingClientRect();
    if (!rect) return;
    panX = event.clientX - rect.left - before.x * zoom;
    panY = event.clientY - rect.top - before.y * zoom;
  }

  function pointerDown(event: PointerEvent, node: GraphNode) {
    if (event.button !== 0) return;
    selected = node.id;
    drag = 'node';
    const world = worldAt(event.clientX, event.clientY);
    dragId = node.id;
    dragDx = world.x - node.x;
    dragDy = world.y - node.y;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function pinDown(event: PointerEvent, node: GraphNode, out: boolean) {
    event.stopPropagation();
    if (event.altKey) {
      doc = { ...doc, cables: breakPinLinks(doc.cables, node.id, 0, out) };
      return;
    }
    const pin = out ? pinOut(node) : pinIn(node);
    wireFrom = { id: node.id, port: 0, out, x: pin.x, y: pin.y };
    drag = 'wire';
    cursor = pin;
  }

  function move(event: PointerEvent) {
    const world = worldAt(event.clientX, event.clientY);
    if (drag === 'node') {
      doc = {
        ...doc,
        nodes: doc.nodes.map((node) => (node.id === dragId ? { ...node, x: world.x - dragDx, y: world.y - dragDy } : node)),
      };
    } else if (drag === 'wire') {
      cursor = world;
    } else if (drag === 'pan') {
      panX += event.movementX;
      panY += event.movementY;
    }
  }

  function up(event: PointerEvent) {
    if (drag === 'wire' && wireFrom) {
      const hit = doc.nodes.find((node) => {
        const pin = wireFrom?.out ? pinIn(node) : pinOut(node);
        return Math.hypot(pin.x - cursor.x, pin.y - cursor.y) < 18 && node.id !== wireFrom?.id;
      });
      if (hit && wireFrom.out) doc = { ...doc, cables: connectPins(doc.cables, wireFrom.id, hit.id) };
      else if (hit) doc = { ...doc, cables: connectPins(doc.cables, hit.id, wireFrom.id) };
      else menu = { x: event.clientX, y: event.clientY, wx: cursor.x, wy: cursor.y, query: '' };
      if (hit) wireFrom = null;
    }
    if (drag !== 'wire') wireFrom = null;
    drag = 'none';
  }

  function backgroundDown(event: PointerEvent) {
    if (event.button === 1 || event.button === 2) {
      drag = 'pan';
      return;
    }
    selected = '';
  }

  function openMenu(event: MouseEvent) {
    event.preventDefault();
    const world = worldAt(event.clientX, event.clientY);
    menu = { x: event.clientX, y: event.clientY, wx: world.x, wy: world.y, query: '' };
    wireFrom = null;
  }

  function onKey(event: KeyboardEvent) {
    const tag = (event.target as HTMLElement | null)?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    if ((event.key === 'Delete' || event.key === 'Backspace') && selected) {
      const next = removeNode(doc.nodes, doc.cables, selected);
      doc = { ...doc, ...next };
      selected = '';
    } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'd' && selected) {
      event.preventDefault();
      const next = duplicateNode(doc.nodes, selected);
      doc = { ...doc, nodes: next.nodes };
      selected = next.id;
    } else if (event.key === 'Home') fit();
    else if (event.key === 'Escape') menu = null;
  }

  function patchParam(id: string, key: string, value: string) {
    doc = {
      ...doc,
      nodes: doc.nodes.map((node) => (node.id === id ? { ...node, params: { ...node.params, [key]: value } } : node)),
    };
  }

  function runSelected() {
    if (!current) return;
    const prompt = doc.nodes.find((node) => node.kind === 'prompt')?.params.text || current.params.text || current.label;
    if (current.kind === 'promote') {
      onpromote?.(current.params.text || current.label, prompt);
      return;
    }
    const job = current.kind === 'palaestra' ? current.params.job || 'image-scout' : current.kind === 'decree' ? 'decree' : current.kind;
    onrun?.(job, prompt);
  }

  function cablePath(cable: GraphCable) {
    const from = doc.nodes.find((node) => node.id === cable.from);
    const to = doc.nodes.find((node) => node.id === cable.to);
    if (!from || !to) return '';
    const a = pinOut(from);
    const b = pinIn(to, cable.toPort);
    return wirePath(a.x, a.y, b.x, b.y);
  }
</script>

<svelte:window onkeydown={onKey} />

<section class="graph">
  <aside class="palette" class:shut={!paletteOpen}>
    <button type="button" onclick={() => (paletteOpen = !paletteOpen)}>{paletteOpen ? 'Add node' : '+'}</button>
    {#if paletteOpen}
      {#each CONDUIT_PALETTE as item (item.kind)}
        <button type="button" draggable="true" ondragstart={(event) => event.dataTransfer?.setData('text/elucifia-kind', item.kind)} onclick={() => spawn(item.kind, 80 + doc.nodes.length * 12, 80)}>
          <i style:background={item.color}></i>{item.label}
        </button>
      {/each}
    {/if}
  </aside>
  <div
    class="stage"
    bind:this={surface}
    role="application"
    aria-label="Blueprint conduit"
    onwheel={onWheel}
    onpointermove={move}
    onpointerup={up}
    onpointerdown={backgroundDown}
    ondblclick={openMenu}
    oncontextmenu={openMenu}
    ondragover={(event) => event.preventDefault()}
    ondrop={(event) => {
      event.preventDefault();
      const kind = event.dataTransfer?.getData('text/elucifia-kind');
      if (!kind) return;
      const world = worldAt(event.clientX, event.clientY);
      spawn(kind, world.x, world.y);
    }}
  >
    <header>
      {#each ['Concept Scout', 'Refine + Grade', 'Hunyuan 3D Mesh', 'Video Previx', 'Voice LoRA', 'Lorekeep Conduit'] as name}
        <button type="button" onclick={() => loadNamed(name)}>{name}</button>
      {/each}
      {#each saved as item (item.id)}
        <button type="button" onclick={() => { doc = item; selected = item.nodes[0]?.id || ''; fit(); }}>{item.name}</button>
      {/each}
      <span class="status">{Math.round(zoom * 100)}% · {doc.nodes.length} nodes · {doc.cables.length} cables</span>
      <button type="button" onclick={() => (zoom = Math.min(4, zoom * 1.1))}>+</button>
      <button type="button" onclick={() => (zoom = Math.max(0.25, zoom / 1.1))}>−</button>
      <button type="button" onclick={fit}>Home</button>
    </header>
    <div class="world" style:transform="translate({panX}px, {panY}px) scale({zoom})">
      <svg class="wires">
        {#each doc.cables as cable (`${cable.from}-${cable.to}`)}
          <path d={cablePath(cable)} />
          <path
            class="hit"
            d={cablePath(cable)}
            role="button"
            tabindex="0"
            aria-label="Remove wire"
            onclick={() => (doc = { ...doc, cables: breakWire(doc.cables, cable.from, cable.to, cable.fromPort, cable.toPort) })}
            onkeydown={(event) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                doc = { ...doc, cables: breakWire(doc.cables, cable.from, cable.to, cable.fromPort, cable.toPort) };
              }
            }}
            oncontextmenu={(event) => {
              event.preventDefault();
              event.stopPropagation();
              doc = { ...doc, cables: breakWire(doc.cables, cable.from, cable.to, cable.fromPort, cable.toPort) };
            }}
          />
        {/each}
        {#if drag === 'wire' && wireFrom}
          <path class="live" d={wirePath(wireFrom.x, wireFrom.y, cursor.x, cursor.y)} />
        {/if}
      </svg>
      {#each doc.nodes as node (node.id)}
        <article
          class="bp-card"
          class:on={node.id === selected}
          class:muted={!node.enabled}
          style:left="{node.x}px"
          style:top="{node.y}px"
          style:--accent={paletteFor(node.kind).color}
          onpointerdown={(event) => pointerDown(event, node)}
        >
          <button type="button" class="pin in" aria-label="Input" onpointerdown={(event) => pinDown(event, node, false)}></button>
          <header><strong>{node.label}</strong><small>{node.kind}</small></header>
          {#if node.kind === 'prompt' || node.kind === 'vault' || node.kind === 'akasha' || node.kind === 'promote'}
            <input value={node.params.text || ''} placeholder={paletteFor(node.kind).hint} oninput={(event) => patchParam(node.id, 'text', (event.currentTarget as HTMLInputElement).value)} />
          {:else if node.kind === 'decree'}
            <ElucifiaSelect value={node.params.model || models[0] || 'gemma4:12b'} options={models} ariaLabel="Model" onchange={(value) => patchParam(node.id, 'model', value)} />
          {:else if node.kind === 'arte'}
            <ElucifiaSelect value={node.params.text || 'effect.spark'} options={['effect.spark', 'sound.chime', 'film.grade', 'light.wash']} ariaLabel="Arte op" onchange={(value) => patchParam(node.id, 'text', value)} />
          {:else}
            <ElucifiaSelect value={node.params.job || jobs[0] || 'image-scout'} options={jobs.length ? jobs : ['image-scout', 'image-refine', 'hunyuan-img2mesh', 'video-previx', 'lora-train-anima']} ariaLabel="Palaestra job" onchange={(value) => patchParam(node.id, 'job', value)} />
          {/if}
          <button type="button" class="pin out" aria-label="Output" onpointerdown={(event) => pinDown(event, node, true)}></button>
        </article>
      {/each}
    </div>
  </div>
  <aside class="inspector" class:shut={!inspectorOpen}>
    <button type="button" onclick={() => (inspectorOpen = !inspectorOpen)}>{inspectorOpen ? 'Inspector' : 'i'}</button>
    {#if inspectorOpen && current}
      <label>Title <input bind:value={current.label} /></label>
      <label class="switch"><input type="checkbox" bind:checked={current.enabled} /> Enabled</label>
      <p>{paletteFor(current.kind).hint}</p>
      <button type="button" onclick={runSelected}>Run node</button>
      <button type="button" onclick={() => { const next = duplicateNode(doc.nodes, current.id); doc = { ...doc, nodes: next.nodes }; selected = next.id; }}>Duplicate</button>
      <button type="button" class="danger" onclick={() => { const next = removeNode(doc.nodes, doc.cables, current.id); doc = { ...doc, ...next }; selected = ''; }}>Delete</button>
      <label>Save as <input bind:value={saveName} placeholder="Graph name" /></label>
      <button type="button" onclick={() => saveName && onpersist?.({ op: 'save', name: saveName, graph: doc })}>Save graph</button>
      {#each saved as item (item.id)}
        <button type="button" onclick={() => onpersist?.({ op: 'delete', id: item.id })}>Delete {item.name}</button>
      {/each}
    {:else if inspectorOpen}
      <p>Select a node. Right-click or double-click the canvas to spawn one.</p>
    {/if}
  </aside>
  {#if menu}
    <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px">
      <input placeholder="Search nodes" bind:value={menu.query} />
      {#each menuItems as item (item.kind)}
        <button type="button" onclick={() => spawn(item.kind, menu?.wx || 80, menu?.wy || 80, wireFrom)}>{item.label}</button>
      {/each}
    </div>
  {/if}
</section>

<style>
  .graph { position: relative; display: grid; grid-template-columns: auto minmax(0, 1fr) auto; min-height: 36rem; height: 100%; }
  .palette, .inspector { display: flex; flex-direction: column; gap: 0.35rem; padding: 0.55rem; background: rgba(22, 15, 29, 0.9); border: 1px solid rgba(201, 111, 208, 0.28); overflow: auto; }
  .palette.shut, .inspector.shut { width: 2.4rem; }
  .palette { width: 13rem; }
  .inspector { width: 16rem; }
  .stage { position: relative; min-width: 0; overflow: hidden; background: radial-gradient(circle at 20% 0%, rgba(201, 111, 208, 0.12), transparent 28rem), #0d0912; }
  .stage > header { position: absolute; z-index: 3; left: 0.5rem; right: 0.5rem; top: 0.45rem; display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; }
  .world { position: absolute; inset: 0; transform-origin: 0 0; }
  .wires { position: absolute; left: 0; top: 0; width: 1px; height: 1px; overflow: visible; pointer-events: none; }
  path { fill: none; stroke: #c96fd0; stroke-width: 2.5; }
  path.live { stroke: #ff8c42; }
  path.hit { stroke: transparent; stroke-width: 14; pointer-events: stroke; }
  .bp-card { position: absolute; width: 228px; min-height: 132px; display: grid; gap: 0.3rem; padding: 0.55rem 0.7rem 0.7rem; border-radius: 0.85rem; border: 1px solid rgba(201, 111, 208, 0.4); border-top: 3px solid var(--accent); background: rgba(18, 12, 24, 0.94); color: #fff4e8; }
  .bp-card.on { box-shadow: 0 0 0 1px #ff8c42; }
  .bp-card.muted { opacity: 0.55; }
  .bp-card header { display: flex; justify-content: space-between; gap: 0.4rem; }
  .bp-card strong { font-size: 0.82rem; }
  .bp-card small, .status, p { color: #a894ab; }
  .pin { position: absolute; top: 42%; width: 0.8rem; height: 0.8rem; border-radius: 99px; border: 2px solid #ff8c42; background: #0d0912; padding: 0; }
  .pin.in { left: -0.45rem; }
  .pin.out { right: -0.45rem; top: 62%; }
  .menu { position: fixed; z-index: 9; display: grid; gap: 0.25rem; width: 14rem; padding: 0.4rem; background: rgba(18, 12, 24, 0.96); border: 1px solid rgba(201, 111, 208, 0.55); border-radius: 0.8rem; box-shadow: 0 14px 34px rgba(0, 0, 0, 0.65); }
  button, input { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: #f4ecf6; border-radius: 0.65rem; padding: 0.28rem 0.45rem; text-align: left; }
  i { display: inline-block; width: 0.55rem; height: 0.55rem; border-radius: 99px; margin-right: 0.3rem; }
  .danger { color: #e06b7a; }
  label { display: grid; gap: 0.2rem; color: #a894ab; }
  .switch { display: flex; align-items: center; }
</style>
