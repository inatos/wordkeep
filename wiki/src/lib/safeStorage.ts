/**
 * Storage wrappers that survive sandboxed iframes without allow-same-origin
 * (Firefox throws on the Window.localStorage / sessionStorage *getter*, not
 * only on getItem/setItem). Falls back to an in-memory Map for the page lifetime.
 */

type SafeStorageApi = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
};

function tryNative(kind: 'localStorage' | 'sessionStorage'): Storage | null {
  try {
    const storage = globalThis[kind];
    const probe = `__wordkeep_wiki_${kind}_probe__`;
    storage.setItem(probe, '1');
    storage.removeItem(probe);
    return storage;
  } catch {
    return null;
  }
}

function makeSafe(kind: 'localStorage' | 'sessionStorage'): SafeStorageApi {
  const memory = new Map<string, string>();
  const native = tryNative(kind);

  return {
    getItem(key: string): string | null {
      if (native) {
        try {
          return native.getItem(key);
        } catch {
          /* use memory */
        }
      }
      return memory.has(key) ? (memory.get(key) ?? null) : null;
    },

    setItem(key: string, value: string): void {
      memory.set(key, value);
      if (!native) return;
      try {
        native.setItem(key, value);
      } catch {
        /* memory only */
      }
    },

    removeItem(key: string): void {
      memory.delete(key);
      if (!native) return;
      try {
        native.removeItem(key);
      } catch {
        /* ignore */
      }
    },
  };
}

export const safeStorage = makeSafe('localStorage');
export const safeSessionStorage = makeSafe('sessionStorage');
