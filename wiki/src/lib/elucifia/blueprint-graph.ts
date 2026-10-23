export type GraphNode = {
  id: string;
  kind: string;
  label: string;
  x: number;
  y: number;
  params: Record<string, string>;
  enabled: boolean;
};

export type GraphCable = {
  from: string;
  to: string;
  fromPort: number;
  toPort: number;
};

export type GraphDoc = {
  id: string;
  name: string;
  nodes: GraphNode[];
  cables: GraphCable[];
};

export const NODE_W = 228;
export const NODE_H = 132;

export const CONDUIT_PALETTE = [
  { kind: 'prompt', label: 'Prompt & @Context', color: '#ff8c42', hint: 'The line you send' },
  { kind: 'vault', label: 'Vault / Lorekeep', color: '#7ec8e3', hint: 'Recall before inventing' },
  { kind: 'decree', label: 'Gemma 4 / Ereshkigal', color: '#c96fd0', hint: 'Decree or chat' },
  { kind: 'arte', label: 'Betwixt Arte', color: '#e879f9', hint: 'effect, sound, film, light' },
  { kind: 'akasha', label: 'Akasha search', color: '#6bcf8e', hint: 'CC0 pack, not a second client' },
  { kind: 'palaestra', label: 'Palaestra GPU', color: '#f59e0b', hint: 'Image, mesh, video, LoRA' },
  { kind: 'promote', label: 'Promote', color: '#f3e6d4', hint: 'Shrift seed or Lorekeep' },
] as const;

export function paletteFor(kind: string) {
  return CONDUIT_PALETTE.find((item) => item.kind === kind) ?? CONDUIT_PALETTE[0];
}

export function wirePath(x1: number, y1: number, x2: number, y2: number): string {
  const dx = Math.max(48, Math.abs(x2 - x1) * 0.45);
  return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
}

export function pinOut(node: { x: number; y: number }, w = NODE_W, h = NODE_H) {
  return { x: node.x + w, y: node.y + h * 0.62 };
}

export function pinIn(node: { x: number; y: number }, port = 0, h = NODE_H) {
  return { x: node.x, y: node.y + h * (port === 0 ? 0.42 : 0.72) };
}

let seq = 1;
export function nextId(prefix = 'n') {
  seq += 1;
  return `${prefix}${seq}`;
}

export function makeNode(kind: string, x: number, y: number, id = nextId()): GraphNode {
  const pal = paletteFor(kind);
  return {
    id,
    kind,
    label: pal.label,
    x,
    y,
    enabled: true,
    params: kind === 'palaestra' ? { job: 'image-scout' } : kind === 'decree' ? { model: 'gemma4:12b' } : { text: '' },
  };
}

export function addNode(nodes: GraphNode[], node: GraphNode): GraphNode[] {
  return [...nodes, node];
}

export function duplicateNode(nodes: GraphNode[], id: string): { nodes: GraphNode[]; id: string } {
  const found = nodes.find((node) => node.id === id);
  if (!found) return { nodes, id };
  const copy = makeNode(found.kind, found.x + 36, found.y + 36);
  copy.label = found.label;
  copy.params = { ...found.params };
  copy.enabled = found.enabled;
  return { nodes: [...nodes, copy], id: copy.id };
}

export function removeNode(nodes: GraphNode[], cables: GraphCable[], id: string) {
  return {
    nodes: nodes.filter((node) => node.id !== id),
    cables: cables.filter((cable) => cable.from !== id && cable.to !== id),
  };
}

export function connectPins(cables: GraphCable[], from: string, to: string, fromPort = 0, toPort = 0): GraphCable[] {
  if (!from || !to || from === to) return cables;
  if (cables.some((cable) => cable.from === from && cable.to === to && cable.fromPort === fromPort && cable.toPort === toPort)) {
    return cables;
  }
  return [...cables, { from, to, fromPort, toPort }];
}

export function breakPinLinks(cables: GraphCable[], id: string, port: number, isOut: boolean): GraphCable[] {
  return cables.filter((cable) => (isOut ? !(cable.from === id && cable.fromPort === port) : !(cable.to === id && cable.toPort === port)));
}

export function breakWire(cables: GraphCable[], from: string, to: string, fromPort = 0, toPort = 0): GraphCable[] {
  return cables.filter((cable) => !(cable.from === from && cable.to === to && cable.fromPort === fromPort && cable.toPort === toPort));
}

export function fitBounds(nodes: GraphNode[], width: number, height: number) {
  if (!nodes.length) return { zoom: 1, panX: 40, panY: 28 };
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of nodes) {
    minX = Math.min(minX, node.x);
    minY = Math.min(minY, node.y);
    maxX = Math.max(maxX, node.x + NODE_W);
    maxY = Math.max(maxY, node.y + NODE_H);
  }
  const pad = 48;
  const bw = Math.max(80, maxX - minX);
  const bh = Math.max(80, maxY - minY);
  const zoom = Math.max(0.25, Math.min(1, Math.min((width - pad * 2) / bw, (height - pad * 2) / bh)));
  return {
    zoom,
    panX: pad - minX * zoom + (width - pad * 2 - bw * zoom) * 0.5,
    panY: pad - minY * zoom + (height - pad * 2 - bh * zoom) * 0.5,
  };
}

export function conduitPreset(name: string): GraphDoc {
  const prompt = makeNode('prompt', 40, 80, 'prompt');
  const vault = makeNode('vault', 320, 40, 'vault');
  const decree = makeNode('decree', 320, 200, 'decree');
  const palaestra = makeNode('palaestra', 620, 120, 'gpu');
  const arte = makeNode('arte', 320, 200, 'arte');
  const promote = makeNode('promote', 900, 120, 'keep');
  if (name === 'Lorekeep Conduit') {
    return {
      id: 'lorekeep',
      name,
      nodes: [prompt, vault, decree, promote],
      cables: connectPins(connectPins(connectPins([], 'prompt', 'vault'), 'vault', 'decree'), 'decree', 'keep'),
    };
  }
  if (name === 'Refine + Grade') {
    palaestra.params = { job: 'image-refine' };
    return { id: 'refine', name, nodes: [prompt, arte, palaestra], cables: connectPins(connectPins([], 'prompt', 'arte'), 'arte', 'gpu') };
  }
  if (name === 'Hunyuan 3D Mesh') {
    palaestra.params = { job: 'hunyuan-img2mesh' };
    return { id: 'mesh', name, nodes: [prompt, palaestra], cables: connectPins([], 'prompt', 'gpu') };
  }
  if (name === 'Video Previx') {
    palaestra.params = { job: 'video-previx' };
    return { id: 'video', name, nodes: [prompt, palaestra], cables: connectPins([], 'prompt', 'gpu') };
  }
  if (name === 'Voice LoRA') {
    palaestra.params = { job: 'lora-train-anima' };
    return { id: 'voice', name, nodes: [prompt, palaestra], cables: connectPins([], 'prompt', 'gpu') };
  }
  palaestra.params = { job: 'image-scout' };
  return {
    id: 'scout',
    name: name || 'Concept Scout',
    nodes: [prompt, decree, palaestra],
    cables: connectPins(connectPins([], 'prompt', 'decree'), 'decree', 'gpu'),
  };
}
