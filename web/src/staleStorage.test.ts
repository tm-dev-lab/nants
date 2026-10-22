import { describe, expect, it } from "vitest";
import { clearStaleStorage, LEGACY_PREFIX, STALE_LOCAL_KEYS, STALE_LOCAL_PREFIXES, STALE_SESSION_PREFIXES } from "./staleStorage";

function fill(storage: Storage, entries: Record<string, string>): void {
  storage.clear();
  for (const [key, value] of Object.entries(entries)) storage.setItem(key, value);
}

const keys = (storage: Storage): string[] => {
  const out: string[] = [];
  for (let i = 0; i < storage.length; i += 1) {
    const key = storage.key(i);
    if (key !== null) out.push(key);
  }
  return out.sort();
};

/** Chave com o nome antigo do produto (montada a partir do prefixo, que só existe em staleStorage.ts). */
const old = (suffix: string): string => `${LEGACY_PREFIX}${suffix}`;

describe("limpeza do armazenamento na carga", () => {
  it("apaga as chaves com o nome antigo (inclusive o ui-state antigo) e preserva o cache nants.ui-state e o resto", () => {
    fill(window.localStorage, {
      [old("sidebar.collapsed")]: "1",
      [old("kanban.columns")]: "[]",
      [old("chat.width")]: "420",
      [old("files.treeHeight")]: "200",
      [old("windows.order")]: "[]",
      [old("windows.modes.p1")]: "{}",
      [old("files.open.p1")]: "1",
      [old("files.width.p2")]: "300",
      [old("ui-state.global")]: "{}",
      [old("ui-state.project.p1")]: "{}",
      "nants.ui-state.global": "{}",
      "nants.ui-state.project.p1": "{}",
      "outro.app": "x",
    });
    fill(window.sessionStorage, {
      [old("view.p1.chat")]: "10",
      [old("view.p1.tab")]: "files",
      [old("ui-state.rascunho")]: "x",
      "nants.ui-state.rascunho": "x",
    });

    expect(clearStaleStorage()).toBe(13);
    expect(keys(window.localStorage)).toEqual(["nants.ui-state.global", "nants.ui-state.project.p1", "outro.app"]);
    expect(keys(window.sessionStorage)).toEqual(["nants.ui-state.rascunho"]);
    window.localStorage.clear();
    window.sessionStorage.clear();
  });

  it("a lista é mínima e explícita: nenhuma entrada cobre o cache do ui-state", () => {
    expect(LEGACY_PREFIX.startsWith("nants")).toBe(false);
    for (const key of STALE_LOCAL_KEYS) expect(key.startsWith("nants.")).toBe(false);
    for (const prefix of [...STALE_LOCAL_PREFIXES, ...STALE_SESSION_PREFIXES]) {
      expect(prefix.endsWith(".")).toBe(true);
      expect("nants.ui-state.global".startsWith(prefix)).toBe(false);
    }
  });

  it("armazenamento indisponível ou que falha não derruba a carga", () => {
    const broken = {
      get length(): number {
        throw new Error("bloqueado");
      },
    } as unknown as Storage;
    expect(clearStaleStorage(broken, null)).toBe(0);
    expect(clearStaleStorage(null, null)).toBe(0);
  });
});
