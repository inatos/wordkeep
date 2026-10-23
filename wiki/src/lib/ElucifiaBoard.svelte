<script lang="ts">
  import { onMount } from 'svelte';
  import ElucifiaCanvas from './elucifia/ElucifiaCanvas.svelte';
  import ElucifiaChatInput from './elucifia/ElucifiaChatInput.svelte';
  import ElucifiaChatStage from './elucifia/ElucifiaChatStage.svelte';
  import ElucifiaDeskStage from './elucifia/ElucifiaDeskStage.svelte';
  import ElucifiaInspector from './elucifia/ElucifiaInspector.svelte';
  import ElucifiaModals from './elucifia/ElucifiaModals.svelte';
  import ElucifiaNotesStage from './elucifia/ElucifiaNotesStage.svelte';
  import ElucifiaPersonasStage from './elucifia/ElucifiaPersonasStage.svelte';
  import ElucifiaSidebar from './elucifia/ElucifiaSidebar.svelte';
  import { renderStudioMarkdown } from './elucifia/markdown';
  import type { GraphDoc } from './elucifia/blueprint-graph';
  import { rippleFromEvent } from './press-ripple';
  import {
    elucifiaPeerEventsUrl,
    fetchElucifiaJobs,
    fetchElucifiaPeer,
    fetchElucifiaStatus,
    fetchElucifiaThreads,
    postElucifia,
    type ElucifiaJob,
    type ElucifiaPeer,
    type ElucifiaThread,
    type ElucifiaTurn,
  } from './api';

  type Mood = 'Attentive' | 'Excited' | 'Skeptical' | 'Tender' | 'Playful';
  type View = 'chats' | 'canvas' | 'dreams' | 'journal' | 'personas' | 'logs' | 'config';
  type CanvasTab = 'graph' | 'gen2d' | 'gal2d' | 'gen3d' | 'gal3d';
  const MOODS: Mood[] = ['Attentive', 'Excited', 'Skeptical', 'Tender', 'Playful'];
  const MODELS = ['gemma4:12b', 'gemma4:e4b', 'gemma4:26b', 'gemma3:12b'];
  const JOBS = ['image-scout', 'image-refine', 'video-previx', 'hunyuan-img2mesh', 'lora-train-anima'];
  const VIEWS: { id: View; label: string; glyph: string; letter: string }[] = [
    { id: 'chats', label: 'Chats', glyph: '✉', letter: 'C' },
    { id: 'canvas', label: 'Canvas', glyph: '◈', letter: 'K' },
    { id: 'dreams', label: 'Dreams', glyph: '☾', letter: 'D' },
    { id: 'journal', label: 'Journal', glyph: '≡', letter: 'J' },
    { id: 'personas', label: 'Personas', glyph: '◉', letter: 'P' },
    { id: 'logs', label: 'Logs', glyph: '⌁', letter: 'L' },
    { id: 'config', label: 'Config', glyph: '⚙', letter: 'G' },
  ];
  function asset(path: string) {
    const env = (import.meta as { env?: { BASE_URL?: string } }).env;
    const base = env?.BASE_URL ?? '/';
    return `${base}${path.replace(/^\//, '')}`;
  }

  const PRESETS = [
    { name: 'Concept Scout', job: 'image-scout' },
    { name: 'Refine + Grade', job: 'image-refine' },
    { name: 'Hunyuan 3D Mesh', job: 'hunyuan-img2mesh' },
    { name: 'Video Previx', job: 'video-previx' },
    { name: 'Voice LoRA', job: 'lora-train-anima' },
  ];
  const CONTEXT = ['@vault', '@lorekeep', '@continuity', '@peer', '@thread', '@page'];

  let mood = $state<Mood>('Attentive');
  let model = $state('gemma4:12b');
  let peerUp = $state(false);
  let voice = $state('hexgrad/Kokoro-82M');
  let lane = $state('stub');
  let loraPairs = $state(0);
  let speaking = $state(false);
  let streaming = $state(false);
  let autoSpeak = $state(false);
  let sfxOn = $state(true);
  let draft = $state('');
  let jobPrompt = $state('');
  let notice = $state('');
  let threads = $state<ElucifiaThread[]>([]);
  let threadId = $state('');
  let turns = $state<ElucifiaTurn[]>([]);
  let jobs = $state<ElucifiaJob[]>([]);
  let busy = $state('');
  let view = $state<View>('chats');
  let railOpen = $state(true);
  let inspector = $state(false);
  let palette = $state(false);
  let paletteQuery = $state('');
  let showThinking = $state(true);
  let sendOnEnter = $state(true);
  let canvasTab = $state<CanvasTab>('graph');
  let preset = $state(PRESETS[0].name);
  let query = $state('');
  let tagFilter = $state('');
  let titleDraft = $state('');
  let glance = $state('');
  let salience = $state('');
  let dreams = $state<{ id: string; title: string; preview: string; body: string; mood: string; tags?: string[]; lane?: string }[]>([]);
  let journal = $state<{ id: string; title: string; preview: string; body: string; mood?: string; tags?: string[]; lane?: string }[]>([]);
  let selectedNote = $state('');
  const CANVAS_TABS: { id: CanvasTab; label: string }[] = [
    { id: 'graph', label: 'Node Graph' },
    { id: 'gen2d', label: 'Generate-2D' },
    { id: 'gal2d', label: 'Gallery-2D' },
    { id: 'gen3d', label: 'Generate-3D' },
    { id: 'gal3d', label: 'Gallery-3D' },
  ];
  let gallery = $state<{ job: string; name: string; kind: string; ext?: string; bytes?: number; modified?: number }[]>([]);
  let personas = $state<{ id: string; name: string; kind: string; description: string; system_prompt: string; bubble: string; voice?: { model?: string; pitch?: number; rate?: number } }[]>([]);
  let groups = $state<{ id: string; name: string; color: string }[]>([]);
  let tags = $state<{ id: string; name: string; color: string }[]>([]);
  let reader = $state<{ title: string; body: string } | null>(null);
  let personaEdit = $state<(typeof personas)[number] | null>(null);
  let book = $state('Wispfire');
  let focusText = $state('');
  let continuity = $state('');
  let room = $state<'vault' | 'workshop'>('vault');
  let modal = $state('');
  let modalTitle = $state('');
  let modalBody = $state('');
  let editingTitle = $state(false);
  let dockHeight = $state(168);
  let branchMenu = $state(false);
  let menuThread = $state('');
  let newGroup = $state('');
  let editTurn = $state('');
  let editIndex = $state(-1);
  let lightbox = $state('');
  let savedGraphs = $state<GraphDoc[]>([]);
  let showAdvanced = $state(false);
  let aspect = $state('1:1');
  let steps = $state(20);
  let seed = $state(0);
  let thinkSeconds = $state(0);
  let nodes = $state([
    { id: 'prompt', label: 'Prompt & @Context', x: 2, y: 30 },
    { id: 'gemma', label: 'Gemma 4 / Ereshkigal', x: 28, y: 8 },
    { id: 'arte', label: 'Arte & Akasha', x: 28, y: 58 },
    { id: 'job', label: 'Palaestra GPU', x: 56, y: 32 },
    { id: 'out', label: 'Artifact', x: 80, y: 32 },
  ]);
  let dragId = $state('');
  let peer = $state({
    reachable: false,
    ollama_up: false,
    model: '',
    context: 0,
    vram_used_mib: 0,
    vram_total_mib: 0,
    gpu_pct: 0,
  });
  let peerLines = $state<string[]>([]);

  const active = $derived(threads.find((t) => t.id === threadId));
  const reconcileLine = $derived(reconcile(model, peer.model, peer.reachable));
  const energy = $derived(Math.max(8, 100 - (peer.gpu_pct || 0)));
  const focus = $derived(Math.min(100, 40 + turns.length * 8));
  const filteredThreads = $derived(
    threads.filter((t) => {
      const blob = `${t.title || ''} ${t.id} ${(t.tags || []).join(' ')}`.toLowerCase();
      if (query && !blob.includes(query.toLowerCase())) return false;
      if (tagFilter && !(t.tags || []).includes(tagFilter)) return false;
      return true;
    }),
  );
  const paletteHits = $derived(
    [...VIEWS.map((v) => v.label), ...MODELS, ...PRESETS.map((p) => p.name)].filter((item) =>
      item.toLowerCase().includes(paletteQuery.toLowerCase()),
    ),
  );

  function reconcile(menu: string, loaded: string, reachable: boolean) {
    if (!reachable) return 'Laptop silent. Chat stays on the local stub.';
    if (!loaded) return `No model is resident. The next send still uses ${menu}.`;
    if (loaded === menu) return `GPU holds ${menu}.`;
    return `GPU holds ${loaded}. The next send still uses ${menu}.`;
  }

  function rememberLine(line: string) {
    if (!line || peerLines[peerLines.length - 1] === line) return;
    peerLines = [...peerLines, line].slice(-40);
  }

  function applyPeer(data: ElucifiaPeer) {
    peer = {
      reachable: Boolean(data.reachable),
      ollama_up: Boolean(data.ollama_up),
      model: data.model || '',
      context: data.context || 0,
      vram_used_mib: data.vram_used_mib || 0,
      vram_total_mib: data.vram_total_mib || 0,
      gpu_pct: data.gpu_pct || 0,
    };
    if (data.events) {
      const seen = new Set(peerLines);
      const next = [...peerLines];
      for (const event of data.events) {
        if (event.line && !seen.has(event.line)) {
          seen.add(event.line);
          next.push(event.line);
        }
      }
      peerLines = next.slice(-40);
    }
  }

  function press(event: MouseEvent) {
    rippleFromEvent(event);
  }

  function tone(freq: number, dur: number, gain: number) {
    if (!sfxOn || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const Ctx = window.AudioContext || (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctx) return;
    const ctx = new Ctx();
    const osc = ctx.createOscillator();
    const amp = ctx.createGain();
    osc.frequency.value = freq;
    osc.type = 'sine';
    const t = ctx.currentTime;
    amp.gain.setValueAtTime(0.0001, t);
    amp.gain.exponentialRampToValueAtTime(gain, t + 0.02);
    amp.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    osc.connect(amp);
    amp.connect(ctx.destination);
    osc.start(t);
    osc.stop(t + dur + 0.02);
    osc.onended = () => ctx.close();
  }

  function playClip(name: 'tick' | 'mood' | 'chime') {
    if (!sfxOn) return;
    const audio = new Audio(asset(`elucifia/sfx/${name}.wav`));
    audio.volume = speaking ? 0.12 : 0.32;
    audio.play().catch(() => {
      if (name === 'tick') tone(880, 0.08, 0.04);
      else if (name === 'chime') tone(660, 0.16, 0.04);
      else tone(520, 0.22, 0.035);
    });
  }

  function speakEnvelope() {
    speaking = true;
    tone(220, 0.55, 0.03);
    window.setTimeout(() => {
      speaking = false;
    }, 700);
  }

  function markdown(text: string) {
    return renderStudioMarkdown(text);
  }

  async function refresh() {
    try {
      const status = await fetchElucifiaStatus();
      if (status.mood && MOODS.includes(status.mood as Mood)) mood = status.mood as Mood;
      if (status.model) model = status.model;
      peerUp = Boolean(status.peer_up);
      voice = status.voice || voice;
      lane = status.chat_lane || lane;
      loraPairs = status.lora_pairs || 0;
      const listed = await fetchElucifiaThreads();
      threads = listed.threads || [];
      const board = await fetchElucifiaJobs();
      jobs = board.jobs || [];
      const shelf = await fetch(`${asset('api/elucifia/')}meta`).then((r) => r.json());
      groups = shelf.groups || [];
      tags = shelf.tags || [];
      personas = shelf.personas || [];
      const persona = shelf.persona || {};
      if (persona.active_book) book = persona.active_book;
      if (persona.focus) focusText = persona.focus;
      if (persona.continuity) continuity = persona.continuity;
      if (typeof shelf.show_thinking === 'boolean') showThinking = shelf.show_thinking;
      if (typeof shelf.send_on_enter === 'boolean') sendOnEnter = shelf.send_on_enter;
      dreams = (await fetch(`${asset('api/elucifia/')}dreams`).then((r) => r.json())).notes || [];
      const pages = await fetch(`${asset('api/elucifia/')}journal`).then((r) => r.json());
      journal = [...(pages.episodic || []), ...(pages.semantic || [])];
      gallery = (await fetch(`${asset('api/elucifia/')}gallery`).then((r) => r.json())).items || [];
      savedGraphs = (await fetch(`${asset('api/elucifia/')}graphs`).then((r) => r.json())).graphs || [];
      const pack = await fetch(`${asset('api/elucifia/')}context`).then((r) => r.json());
      if (!glance && pack.glance) glance = pack.glance;
    } catch (err) {
      notice = err instanceof Error ? err.message : 'Elucifia API is not up yet.';
    }
  }

  function selectThread(id: string) {
    threadId = id;
    const found = threads.find((t) => t.id === id);
    turns = found?.turns ? [...found.turns] : [];
    titleDraft = found?.title || found?.id || '';
  }

  async function send(messageOverride?: string) {
    const message = (messageOverride ?? draft).trim();
    if (!message || streaming) return;
    if (!messageOverride) draft = '';
    rememberLine(`POST /api/chat ${model}`);
    const refs = CONTEXT.filter((chip) => message.includes(chip));
    turns = [...turns, { role: 'user', content: message, context_refs: refs }, { role: 'assistant', content: '' }];
    const index = turns.length - 1;
    streaming = true;
    thinkSeconds = 0;
    const clock = window.setInterval(() => {
      thinkSeconds += 1;
    }, 1000);
    let ticked = false;
    try {
      if (refs.length) {
        const pack = await postElucifia<{ glance?: string; salience?: string }>('context', { query: message });
        glance = pack.glance || '';
        salience = pack.salience || '';
      }
      const res = await fetch(`${asset('api/elucifia/')}chat`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ message, model, thread_id: threadId, stream: true }),
      });
      if (!res.ok || !res.body) {
        notice = `Chat failed (${res.status})`;
        return;
      }
      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = '';
      while (true) {
        const step = await reader.read();
        if (step.done) break;
        buf += dec.decode(step.value, { stream: true });
        const parts = buf.split('\n\n');
        buf = parts.pop() ?? '';
        for (const part of parts) {
          const line = part.split('\n').find((l) => l.startsWith('data:'));
          if (!line) continue;
          const data = JSON.parse(line.slice(5).trim()) as {
            token?: string;
            done?: boolean;
            reply?: string;
            mood?: string;
            thread_id?: string;
            lane?: string;
            stub?: boolean;
            error?: string;
          };
          if (data.error) notice = data.error;
          if (data.token) {
            if (!ticked) {
              ticked = true;
              playClip('tick');
            }
            const prior = turns[index]?.content ?? '';
            turns = turns.map((turn, n) => (n === index ? { ...turn, content: `${prior}${data.token}` } : turn));
          }
          if (data.done) {
            if (data.reply) {
              turns = turns.map((turn, n) =>
                n === index ? { ...turn, content: data.reply || '', mood: data.mood } : turn,
              );
            }
            if (data.thread_id) threadId = data.thread_id;
            if (data.lane) lane = data.lane;
            if (data.mood && MOODS.includes(data.mood as Mood)) {
              const next = data.mood as Mood;
              if (next !== mood) playClip('mood');
              mood = next;
            }
            if (data.stub) notice = 'Peer silent. This reply is the local stub.';
            if (autoSpeak) speakEnvelope();
          }
        }
      }
      await refresh();
      if (threadId) selectThread(threadId);
    } catch (err) {
      notice = err instanceof Error ? err.message : 'stream failed';
    } finally {
      window.clearInterval(clock);
      streaming = false;
    }
  }

  function onKey(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey && sendOnEnter) {
      event.preventDefault();
      void send();
    }
  }

  function onGlobal(event: KeyboardEvent) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      palette = !palette;
    }
    if (event.key === 'Escape') palette = false;
  }

  function pickPalette(item: string) {
    const viewHit = VIEWS.find((v) => v.label === item);
    if (viewHit) view = viewHit.id;
    else if (MODELS.includes(item)) model = item;
    else {
      const found = PRESETS.find((p) => p.name === item);
      if (found) {
        preset = found.name;
        view = 'canvas';
        canvasTab = 'graph';
      }
    }
    palette = false;
  }

  function addChip(chip: string) {
    draft = draft.includes(chip) ? draft : `${draft}${draft ? ' ' : ''}${chip} `;
  }

  async function setMood(next: Mood, event: MouseEvent) {
    press(event);
    if (next !== mood) playClip('mood');
    mood = next;
    await postElucifia('mood', { mood: next });
  }

  async function speakText(text: string, event?: MouseEvent) {
    if (event) press(event);
    await postElucifia('speak', { text: text || 'Seraphina Elucifia' });
    speakEnvelope();
  }

  async function distill(event?: MouseEvent) {
    if (event) press(event);
    if (!threadId) {
      notice = 'Open a thread before distill.';
      return;
    }
    busy = 'distill';
    try {
      await postElucifia('distill', { thread_id: threadId });
      playClip('chime');
      await refresh();
    } catch (err) {
      notice = err instanceof Error ? err.message : 'distill failed';
    } finally {
      busy = '';
    }
  }

  async function dream(event?: MouseEvent) {
    if (event) press(event);
    if (!threadId) {
      notice = 'Open a thread before a dream.';
      return;
    }
    busy = 'dream';
    try {
      await postElucifia('dream', { thread_id: threadId, stub: !peerUp });
      playClip('chime');
      notice = 'Dream written under Lorekeep reflections.';
      await refresh();
    } catch (err) {
      notice = err instanceof Error ? err.message : 'dream failed';
    } finally {
      busy = '';
    }
  }

  async function runJob(name: string, event?: MouseEvent) {
    if (event) press(event);
    busy = name;
    try {
      await postElucifia('jobs', { job: name, prompt: jobPrompt || draft, live: false });
      playClip('chime');
      await refresh();
    } catch (err) {
      notice = err instanceof Error ? err.message : 'job failed';
    } finally {
      busy = '';
    }
  }

  async function persistGraph(body: Record<string, unknown>) {
    const saved = await postElucifia<{ graphs?: GraphDoc[] }>('graphs', body);
    savedGraphs = saved.graphs || [];
    playClip('tick');
  }

  async function promoteGraph(title: string, body: string) {
    await fetch(asset('api/shrift'), {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ title, body, tags: ['elucifia'], source: 'elucifia' }),
    });
    playClip('chime');
    notice = 'Promoted to Shrift.';
  }

  async function saveTitle() {
    if (!threadId) return;
    await postElucifia('threads', { id: threadId, title: titleDraft });
    await refresh();
    selectThread(threadId);
  }

  async function fork() {
    if (!threadId) return;
    const next = await postElucifia<ElucifiaThread>('branches', { thread_id: threadId, op: 'fork', name: 'aside' });
    await refresh();
    if (next.id) selectThread(next.id);
  }

  async function useBranch(id: string) {
    if (!threadId) return;
    await postElucifia('branches', { thread_id: threadId, op: 'switch', branch_id: id });
    await refresh();
    selectThread(threadId);
  }

  async function toggleTag(name: string) {
    if (!threadId) return;
    await postElucifia('threads', { id: threadId, toggle_tag: name });
    await refresh();
    selectThread(threadId);
  }

  async function savePersona() {
    if (!personaEdit) return;
    await postElucifia('personas', {
      ...personaEdit,
      voice: personaEdit.voice || { model: voice, pitch: 1, rate: 1 },
    });
    personaEdit = null;
    await refresh();
  }

  function onNodeDown(id: string, event: PointerEvent) {
    dragId = id;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function onNodeMove(event: PointerEvent) {
    if (!dragId) return;
    const board = (event.currentTarget as HTMLElement).getBoundingClientRect();
    nodes = nodes.map((node) =>
      node.id === dragId
        ? {
            ...node,
            x: Math.max(0, Math.min(82, ((event.clientX - board.left) / board.width) * 100 - 9)),
            y: Math.max(0, Math.min(78, ((event.clientY - board.top) / board.height) * 100 - 8)),
          }
        : node,
    );
  }

  async function removeThread(id: string) {
    await postElucifia('threads', { id, op: 'delete' });
    if (threadId === id) {
      threadId = '';
      turns = [];
    }
    await refresh();
  }

  async function moveThread(id: string, groupId: string) {
    await postElucifia('threads', { id, group_id: groupId });
    menuThread = '';
    await refresh();
  }

  async function addGroup() {
    const name = newGroup.trim();
    if (!name) return;
    await postElucifia('groups', { name });
    newGroup = '';
    await refresh();
  }

  async function dropGroup(id: string) {
    await postElucifia('groups', { op: 'delete', id });
    await refresh();
  }

  async function renamePath(id: string) {
    const name = window.prompt('Path name', id) || '';
    if (!name || !threadId) return;
    await postElucifia('branches', { thread_id: threadId, op: 'rename', branch_id: id, name });
    branchMenu = false;
    await refresh();
    selectThread(threadId);
  }

  async function dropPath(id: string) {
    if (!threadId) return;
    await postElucifia('branches', { thread_id: threadId, op: 'delete', branch_id: id });
    branchMenu = false;
    await refresh();
    selectThread(threadId);
  }

  function openNote(note: { id?: string; title: string; body?: string; preview?: string }, kind: string) {
    selectedNote = note.id || '';
    view = kind === 'dream' ? 'dreams' : 'journal';
  }

  async function saveNote(kind: 'dreams' | 'journal', payload: { id: string; title: string; markdown: string; lane: string }) {
    const saved = await postElucifia<{ id?: string }>(kind, payload);
    selectedNote = saved.id || payload.id;
    await refresh();
  }

  async function dropNote(kind: 'dreams' | 'journal', payload: { id: string; lane: string }) {
    await postElucifia(kind, { op: 'delete', ...payload });
    selectedNote = '';
    await refresh();
  }

  async function setThreadTags(next: string[]) {
    if (!threadId) return;
    const current = active?.tags || [];
    for (const name of next.filter((tag) => !current.includes(tag))) await toggleTag(name);
    for (const name of current.filter((tag) => !next.includes(tag))) await toggleTag(name);
  }

  async function paintTag(op: string, name: string, color: string, next?: string) {
    const found = tags.find((tag) => tag.name === name);
    if (op === 'rename' && found && next) {
      await postElucifia('tags', { op: 'rename', id: found.id, name: next, color });
    } else {
      await postElucifia('tags', { op: 'upsert', name, color });
    }
    await refresh();
  }

  async function saveSettings() {
    await postElucifia('meta', { show_thinking: showThinking, send_on_enter: sendOnEnter });
    await refresh();
  }

  function openProfile(name: string) {
    const found = personas.find((p) => p.name === name) || personas[0];
    modal = 'profile';
    modalTitle = found?.name || name;
    modalBody = found?.system_prompt || found?.description || '';
  }

  function beginEdit(index: number) {
    editIndex = index;
    editTurn = turns[index]?.content || '';
    modal = 'message';
  }

  async function saveTurn() {
    if (!threadId || editIndex < 0) return;
    await postElucifia('threads', { id: threadId, turn_index: editIndex, content: editTurn, truncate: true });
    modal = '';
    await refresh();
    selectThread(threadId);
  }

  function startDock(event: PointerEvent) {
    const startY = event.clientY;
    const startH = dockHeight;
    const move = (ev: PointerEvent) => {
      dockHeight = Math.max(120, Math.min(420, startH + (startY - ev.clientY)));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function activeJob() {
    return PRESETS.find((p) => p.name === preset)?.job || 'image-scout';
  }

  onMount(() => {
    const params = new URLSearchParams(location.search);
    const asked = params.get('studio');
    if (asked === 'canvas' || asked === 'dreams' || asked === 'journal' || asked === 'personas' || asked === 'logs' || asked === 'config' || asked === 'chats') {
      view = asked;
    }
    const canvas = params.get('canvas');
    if (canvas === 'graph' || canvas === 'gen2d' || canvas === 'gal2d' || canvas === 'gen3d' || canvas === 'gal3d') {
      canvasTab = canvas;
      view = 'canvas';
    }
    const askedModal = params.get('modal');
    if (askedModal === 'profile' || askedModal === 'dream' || askedModal === 'models') {
      modal = askedModal;
      modalTitle = 'Seraphina Elucifia';
      modalBody = 'Lorekeep of the Atheneum. The vault keeps Wispfire. The workshop keeps the laptop.';
    }
    if (params.get('rail') === 'closed') railOpen = false;
    if (params.get('inspector') === '1') inspector = true;
    if (params.get('preview') === '1') {
      titleDraft = 'Atheneum';
      turns = [
        { role: 'user', content: 'The vault is open.' },
        { role: 'assistant', content: 'Wispfire is the active book. I am here.', mood: 'Attentive' },
      ];
    }
    void refresh();
    void fetchElucifiaPeer().then(applyPeer).catch(() => {
      peer = { ...peer, reachable: false };
    });
    window.addEventListener('keydown', onGlobal);
    const seen = new Set<number>();
    const source = new EventSource(elucifiaPeerEventsUrl());
    source.onmessage = (ev) => {
      let data: ElucifiaPeer;
      try {
        data = JSON.parse(ev.data) as ElucifiaPeer;
      } catch {
        return;
      }
      if (data.kind === 'facts') {
        applyPeer({ ...data, reachable: data.reachable !== false });
        return;
      }
      if (data.kind === 'line' && data.line) {
        if (data.seq !== undefined && seen.has(data.seq)) return;
        if (data.seq !== undefined) seen.add(data.seq);
        rememberLine(data.line);
        return;
      }
      if (data.reachable === false) peer = { ...peer, reachable: false };
    };
    return () => {
      source.close();
      window.removeEventListener('keydown', onGlobal);
    };
  });
</script>


<div
  class="studio elucifia-studio"
  class:rail-open={railOpen}
  onclick={(event) => {
    const target = event.target as HTMLElement | null;
    if (target?.closest('button')) rippleFromEvent(event);
  }}
  data-mood={mood}
  data-speaking={speaking}
  data-streaming={streaming}
  data-testid="elucifia-board"
>
  <ElucifiaSidebar
    {railOpen}
    {view}
    views={VIEWS}
    bind:query
    bind:newGroup
    bind:tagFilter
    {groups}
    {tags}
    threads={filteredThreads}
    {threadId}
    {dreams}
    {journal}
    {personas}
    {peerLines}
    bind:model
    models={MODELS}
    bind:showThinking
    bind:sendOnEnter
    {menuThread}
    {mood}
    ontoggle={() => (railOpen = !railOpen)}
    onview={(id) => (view = id as View)}
    onnewchat={() => { threadId = ''; turns = []; titleDraft = 'New chat'; }}
    onaddgroup={addGroup}
    ondropgroup={dropGroup}
    onselect={selectThread}
    onmenu={(id) => (menuThread = id)}
    onmove={moveThread}
    ondelete={removeThread}
    onnote={openNote}
    onpersona={openProfile}
    onnewpersona={() => { personaEdit = { id: '', name: 'New persona', kind: 'ai', description: '', system_prompt: '', bubble: '#ff8c42' }; modal = 'persona'; }}
    onmodels={() => (modal = 'models')}
    onclear={() => { turns = []; draft = ''; }}
  />

  <main class="stage">
    <div class="wash" aria-hidden="true">
      <span class="blob a"></span>
      <span class="blob b"></span>
      <span class="stars"></span>
      <span class="floor"><span class="grid"></span></span>
      <span class="hum"></span>
      <span class="hum r2"></span>
      <span class="node"></span>
      <span class="scan"></span>
    </div>
    <div class="stage-body">
    {#if view === 'canvas'}
      <ElucifiaCanvas
        tabs={CANVAS_TABS}
        {canvasTab}
        presets={PRESETS}
        {preset}
        {busy}
        {nodes}
        bind:jobPrompt
        bind:aspect
        bind:steps
        bind:seed
        bind:showAdvanced
        {gallery}
        models={MODELS}
        jobNames={PRESETS.map((item) => item.job)}
        {savedGraphs}
        thumb={(job, name) => asset(`api/elucifia/gallery/${job}/${name}`)}
        ontab={(id) => (canvasTab = id as CanvasTab)}
        onpreset={(name) => (preset = name)}
        onqueue={(e) => runJob(activeJob(), e)}
        onrun={(job) => runJob(job)}
        onnodedown={onNodeDown}
        onnodemove={onNodeMove}
        onnodeup={() => (dragId = '')}
        onopen={(name) => (lightbox = name)}
        onpersist={persistGraph}
        onpromote={promoteGraph}
      />
    {:else if view === 'dreams' || view === 'journal'}
      <ElucifiaNotesStage
        kind={view}
        notes={view === 'dreams' ? dreams : journal}
        selectedId={selectedNote}
        palette={tags}
        onsave={(payload) => saveNote(view, payload)}
        ondelete={(payload) => dropNote(view, payload)}
        ondream={() => dream()}
        ondistill={() => distill()}
      />
    {:else if view === 'personas'}
      <ElucifiaPersonasStage
        {personas}
        onsave={(persona) => { personaEdit = persona; void savePersona(); }}
        onspeak={(text) => speakText(text)}
      />
    {:else if view === 'logs' || view === 'config'}
      <ElucifiaDeskStage
        mode={view}
        lines={peerLines}
        {jobs}
        bind:model
        models={MODELS}
        bind:voice
        bind:showThinking
        bind:sendOnEnter
        bind:autoSpeak
        bind:sfxOn
        {continuity}
        vault={glance}
        onclear={() => { peerLines = []; }}
        onsave={saveSettings}
      />
    {:else}
      <ElucifiaChatStage
        {editingTitle}
        bind:titleDraft
        title={active?.title || ''}
        branch={active?.current_branch || 'main'}
        {room}
        {book}
        {turns}
        {streaming}
        {thinkSeconds}
        {showThinking}
        {model}
        {mood}
        {lane}
        {markdown}
        tags={active?.tags || []}
        palette={tags}
        ontags={setThreadTags}
        ontagpalette={paintTag}
        onbranch={() => (branchMenu = !branchMenu)}
        onedit={() => (editingTitle = true)}
        onsave={() => { editingTitle = false; void saveTitle(); }}
        onroom={(next) => { room = next; inspector = true; }}
        onpalette={() => (palette = true)}
        onprofile={openProfile}
        onspeak={(text, event) => speakText(text, event)}
        onfork={beginEdit}
        onregen={(text) => send(text)}
        onpin={(event) => distill(event)}
      />
      <ElucifiaChatInput
        bind:draft
        bind:model
        models={MODELS}
        branch={active?.current_branch || 'main'}
        branches={active?.branches || []}
        {branchMenu}
        chips={CONTEXT}
        {dockHeight}
        {streaming}
        {autoSpeak}
        onbranch={() => (branchMenu = !branchMenu)}
        onswitch={useBranch}
        onrename={renamePath}
        ondelete={dropPath}
        onfork={fork}
        onchip={addChip}
        onmodels={() => (modal = 'models')}
        onspeak={() => (autoSpeak = !autoSpeak)}
        onsend={() => send()}
        onkey={onKey}
        onresize={startDock}
      />
    {/if}
    </div>
  </main>

  {#if view !== 'canvas'}
    <ElucifiaInspector
      open={inspector}
      {room}
      {book}
      {focusText}
      {continuity}
      {mood}
      {energy}
      {focus}
      {glance}
      {salience}
      {loraPairs}
      {busy}
      moods={MOODS}
      {reconcileLine}
      {peer}
      {voice}
      {jobs}
      {peerLines}
      onroom={(next) => (room = next)}
      onclose={() => (inspector = false)}
      onopen={() => (inspector = true)}
      onmood={(next, event) => setMood(next as Mood, event)}
      ondistill={(event) => distill(event)}
      ondream={dream}
    />
  {/if}

  <ElucifiaModals
    {modal}
    {modalTitle}
    {modalBody}
    models={MODELS}
    bind:personaEdit
    bind:editTurn
    {book}
    {focusText}
    {mood}
    {lightbox}
    lightboxSrc={/\.(png|jpe?g|webp)$/i.test(lightbox) ? asset(`api/elucifia/gallery/${lightbox}`) : ''}
    {palette}
    bind:paletteQuery
    {paletteHits}
    {markdown}
    onclose={() => (modal = '')}
    onmodel={(next) => { model = next; modal = ''; }}
    onsave={saveTurn}
    onsavepersona={savePersona}
    onlightbox={() => (lightbox = '')}
    onpalette={() => (palette = false)}
    onpick={pickPalette}
  />
  {#if notice}<p class="toast">{notice}</p>{/if}
</div>

<style>
  .studio {
    --obsidian: #0d0912;
    --glass: rgba(22, 15, 29, 0.88);
    --serif: "Cormorant Garamond", Cinzel, "Iowan Old Style", Palatino, Georgia, serif;
    position: relative;
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: stretch;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    color: #f4ecf6;
    background: var(--obsidian);
    font-family: "IBM Plex Sans", sans-serif;
  }
  .studio[data-mood="Attentive"] { --aurora-a: #7ec8e3; --aurora-b: #c96fd0; }
  .studio[data-mood="Excited"] { --aurora-a: #f59e0b; --aurora-b: #e879f9; }
  .studio[data-mood="Skeptical"] { --aurora-a: #94a3b8; --aurora-b: #a855f7; }
  .studio[data-mood="Tender"] { --aurora-a: #f472b6; --aurora-b: #c084fc; }
  .studio[data-mood="Playful"] { --aurora-a: #6bcf8e; --aurora-b: #e879f9; }
  .drawer { position: relative; width: 18rem; min-height: 100%; padding: 0.8rem; background: var(--glass); backdrop-filter: blur(14px); border-right: 1px solid rgba(201, 111, 208, 0.28); overflow: auto; }
  .drawer.collapsed { width: 3.5rem; }
  .edge { position: absolute; right: -0.8rem; top: 1.2rem; width: 1.6rem; height: 1.6rem; border-radius: 999px; border: 1px solid #c96fd0; background: #160f1d; color: #f4ecf6; z-index: 2; }
  .tabs { display: grid; grid-template-columns: 1fr 1fr; gap: 0.35rem; margin-bottom: 0.7rem; }
  .drawer.collapsed .tabs { grid-template-columns: 1fr; }
  .tabs button, .new, .card, .filters button, .canvas-tabs button, .presets button, .quick button, .moods button, .sendrow button, .toolbar button, .pill { border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.55); color: inherit; border-radius: 0.7rem; }
  .tabs button { display: grid; justify-items: center; padding: 0.35rem; font-size: 0.72rem; }
  .tabs em { font-style: normal; }
  .tabs button.on, .card.on, .filters button.on, .canvas-tabs button.on, .presets button.on, .quick button.on, .moods button.on, .sendrow button.on { border-color: #ff8c42; box-shadow: 0 0 16px rgba(255, 140, 66, 0.35); }
  .new, .find, .row input, select, textarea, .title-edit { width: 100%; margin: 0.25rem 0; }
  .find, .row input, select, textarea, .title-edit { border-radius: 0.7rem; border: 1px solid rgba(201, 111, 208, 0.35); background: rgba(13, 9, 18, 0.72); color: inherit; padding: 0.45rem 0.6rem; }
  .row, .chip-row, .filters, .quick, .moods, .toolbar, .sendrow, .canvas-tabs, .presets { display: flex; flex-wrap: wrap; gap: 0.3rem; align-items: center; }
  .card { display: grid; width: 100%; text-align: left; padding: 0.45rem 0.55rem; margin: 0.25rem 0; }
  .card span, .lede, .mark-line, .think { color: #cbb8d0; font-size: 0.78rem; }
  .chip-row i { width: 0.55rem; height: 0.55rem; border-radius: 99px; display: inline-block; }
  .stage { isolation: isolate; position: relative; display: flex; flex-direction: column; min-width: 0; min-height: 0; height: 100%; overflow: hidden; padding: 0.75rem 2.1rem 0.6rem; }
  .stage-body { position: relative; z-index: 1; display: flex; flex-direction: column; flex: 1 1 auto; min-height: 0; min-width: 0; overflow: hidden; }
  .wash { position: absolute; inset: 0; z-index: 0; overflow: hidden; pointer-events: none; }
  .stage > :global(:not(.wash)) { position: relative; z-index: 1; }
  .blob, .stars, .floor, .grid, .hum, .node, .scan { position: absolute; pointer-events: none; }
  .blob { border-radius: 50%; filter: blur(28px); animation: aurora-drift 14s ease-in-out infinite; }
  .blob.a { width: 52rem; height: 26rem; left: -18%; top: -18%; background: radial-gradient(circle, var(--aurora-a, #7ec8e3), transparent 62%); }
  .blob.b { width: 44rem; height: 26rem; right: -16%; top: -8%; background: radial-gradient(circle, var(--aurora-b, #c96fd0), transparent 58%); animation-duration: 18s; animation-delay: -6s; }
  .stars { inset: -8%; background-image: radial-gradient(2px 2px at 12% 18%, rgba(255, 244, 232, 0.9), transparent), radial-gradient(1.5px 1.5px at 28% 62%, rgba(201, 111, 208, 0.8), transparent), radial-gradient(2px 2px at 44% 28%, rgba(255, 255, 255, 0.8), transparent), radial-gradient(1.5px 1.5px at 61% 74%, rgba(126, 200, 227, 0.75), transparent), radial-gradient(2px 2px at 73% 22%, rgba(255, 244, 232, 0.85), transparent), radial-gradient(1.5px 1.5px at 86% 58%, rgba(232, 121, 249, 0.7), transparent), radial-gradient(1.5px 1.5px at 18% 84%, rgba(255, 255, 255, 0.65), transparent), radial-gradient(2px 2px at 92% 38%, rgba(255, 140, 66, 0.75), transparent); animation: star-breathe 9s ease-in-out infinite alternate; }
  .floor { inset: -10% -6% -4%; transform: perspective(900px) rotateX(62deg) scale(2.15) translateY(18%); transform-origin: center 88%; mask-image: radial-gradient(ellipse at center, #000 32%, transparent 74%); animation: floor-breathe 9s ease-in-out infinite; }
  .grid { inset: 0; background: linear-gradient(rgba(201, 111, 208, 0.34) 1px, transparent 1px), linear-gradient(90deg, rgba(201, 111, 208, 0.34) 1px, transparent 1px); background-size: 48px 48px; animation: grid-drift 16s linear infinite; }
  .hum { right: 2%; top: 12%; width: min(46vw, 30rem); height: min(46vw, 30rem); border-radius: 50%; border: 1px solid color-mix(in srgb, var(--aurora-b, #c96fd0) 55%, transparent); box-shadow: 0 0 28px color-mix(in srgb, var(--aurora-a, #7ec8e3) 32%, transparent); animation: ring-hum 11s linear infinite; }
  .hum.r2 { right: 12%; top: 26%; width: min(28vw, 18rem); height: min(28vw, 18rem); opacity: 0.7; animation-delay: -2.6s; }
  .node { right: 18%; top: 20%; width: 0.85rem; height: 0.85rem; border-radius: 50%; background: radial-gradient(circle at 35% 30%, #fff 0%, #e879f9 42%, #7c3aed 100%); box-shadow: 0 0 18px #e879f9; animation: node-orbit 11s linear infinite; }
  .scan { inset: 0; background: repeating-linear-gradient(transparent 0 3px, rgba(201, 111, 208, 0.14) 3px 4px); opacity: 0.45; animation: scan-drift 10s linear infinite; }
  @keyframes aurora-drift { 0%, 100% { transform: translate3d(0, 0, 0) scale(1); } 50% { transform: translate3d(12%, 10%, 0) scale(1.12); } }
  @keyframes star-breathe { from { opacity: 0.55; transform: scale(1); } to { opacity: 1; transform: scale(1.06); } }
  @keyframes floor-breathe { 0%, 100% { opacity: 0.55; } 50% { opacity: 1; } }
  @keyframes grid-drift { from { background-position: 0 0, 0 0; } to { background-position: 48px 48px, 48px 0; } }
  @keyframes ring-hum { 0%, 100% { opacity: 0.4; } 50% { opacity: 1; } }
  @keyframes node-orbit { from { transform: rotate(0deg) translateX(4.6rem) rotate(0deg); } to { transform: rotate(360deg) translateX(4.6rem) rotate(-360deg); } }
  @keyframes scan-drift { from { background-position: 0 0; } to { background-position: 0 -48px; } }
  @media (prefers-reduced-motion: reduce) { .blob, .stars, .floor, .grid, .hum, .node, .scan { animation: none; } }
  .title { text-align: center; position: relative; }
  .title h1, .mark, .welcome h2, .glass h2 { font-family: var(--serif); font-weight: 500; letter-spacing: 0.04em; }
  .mark { background: none; border: 0; color: #fff4e8; font-size: 1.7rem; }
  .title p { margin: 0; color: #d7c4de; }
  .quick { position: absolute; right: 0; top: 0; }
  .stream { max-width: 850px; margin: 0 auto; padding-bottom: 1rem; display: grid; gap: 0.8rem; }
  .welcome { text-align: center; padding: 2rem 0 1rem; }
  .welcome span { font-size: 2rem; color: #ff8c42; text-shadow: 0 0 18px #ff8c42; }
  .msg { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 0.6rem; }
  .msg.user { direction: rtl; }
  .msg.user .bubble { direction: ltr; }
  .avatar { width: 2.4rem; height: 2.4rem; border-radius: 999px; border: 0; color: #1a0d12; background: var(--bubble); box-shadow: 0 0 16px var(--bubble); }
  .bubble { position: relative; padding: 0.75rem 0.9rem 1.3rem; border-radius: 1rem; background: rgba(22, 15, 29, 0.78); border: 1px solid rgba(255, 140, 66, 0.2); }
  .acts { display: flex; flex-wrap: wrap; gap: 0.25rem; opacity: 0; }
  .bubble:hover .acts, .bubble:focus-within .acts { opacity: 1; }
  .mark-line { position: absolute; right: 0.7rem; bottom: 0.25rem; opacity: 0; font-family: "IBM Plex Mono", monospace; }
  .bubble:hover .mark-line { opacity: 1; }
  .shimmer { height: 0.35rem; border-radius: 99px; background: linear-gradient(90deg, transparent, #ff8c42, transparent); animation: slide 1.2s linear infinite; }
  @keyframes slide { from { transform: translateX(-30%); } to { transform: translateX(30%); } }
  .dock { position: sticky; bottom: 0.4rem; width: min(880px, calc(100% - 1rem)); margin: 0.4rem auto 0; display: grid; gap: 0.35rem; padding: 0.35rem 0.7rem 0.55rem; border-radius: 1.1rem; background: var(--glass); backdrop-filter: blur(14px); border: 1px solid rgba(255, 140, 66, 0.35); box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35); }
  .resize { height: 0.45rem; cursor: ns-resize; border-radius: 99px; background: rgba(255, 140, 66, 0.45); }
  .dock textarea { min-height: 2.4rem; resize: none; }
  .grow { flex: 1; }
  .inspector { width: 16rem; padding: 0.8rem; background: var(--glass); border-left: 1px solid rgba(201, 111, 208, 0.28); }
  .inspector .quick { position: static; }
  .graph { position: relative; min-height: 420px; border-radius: 1rem; background: rgba(13, 9, 18, 0.45); overflow: hidden; }
  .graph svg { position: absolute; inset: 0; width: 100%; height: 100%; }
  .graph path { fill: none; stroke: #c96fd0; stroke-width: 0.6; vector-effect: non-scaling-stroke; }
  .graph path.pulse { stroke: #ff8c42; }
  .node { position: absolute; max-width: 9rem; padding: 0.45rem; border-radius: 0.8rem; background: rgba(22, 15, 29, 0.9); color: inherit; border: 1px solid #ff8c42; }
  .split { display: grid; grid-template-columns: 1fr 1fr; gap: 0.8rem; }
  .preview, .gallery button, .glass { background: var(--glass); border: 1px solid rgba(201, 111, 208, 0.35); border-radius: 1rem; }
  .preview { min-height: 320px; display: grid; place-items: center; padding: 1rem; }
  .gallery { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 0.6rem; }
  .gallery button { padding: 1rem; text-align: left; color: inherit; }
  .veil { position: fixed; inset: 0; background: rgba(8, 5, 12, 0.62); display: grid; place-items: center; z-index: 20; }
  .glass { width: min(640px, 92vw); max-height: 80vh; overflow: auto; padding: 1rem 1.1rem; backdrop-filter: blur(16px); }
  .prose { font-family: var(--serif); font-size: 1.15rem; line-height: 1.45; }
  .log { white-space: pre-wrap; font-family: "IBM Plex Mono", monospace; font-size: 0.75rem; }
  .toast { position: fixed; bottom: 1rem; left: 50%; transform: translateX(-50%); background: #160f1d; padding: 0.4rem 0.7rem; border-radius: 999px; }
  .danger { color: #e06b7a; }
  .bar { height: 0.3rem; background: linear-gradient(90deg, #ff8c42, #c96fd0); }
  .menu { display: grid; gap: 0.2rem; }
  .switch { display: flex; gap: 0.4rem; align-items: center; }
  @media (max-width: 1100px) {
    .split { grid-template-columns: 1fr; }
  }
</style>
