import { describe, expect, it } from "vitest";
import { UI_SECTIONS, UiStateSync } from "../desktop/uiState";
import { fakeUiStateServer, memoryBroadcastHub, memoryStorage } from "../desktop/test-utils";
import { getLanguage, LANG_STORAGE_KEY } from "./index";
import { bindLanguageToUiState, chooseLanguage } from "./uiStateBinding";

async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
}

function makeGlobalSync(server: ReturnType<typeof fakeUiStateServer>, hub = memoryBroadcastHub()) {
  return new UiStateSync(
    { kind: "global" },
    { fetchImpl: server.fetchImpl, storage: null, createChannel: hub.create, setTimer: () => 0, clearTimer: () => undefined },
  );
}

describe("idioma no estado da interface", () => {
  it("o servidor vence ao carregar e sobrescreve o espelho local", async () => {
    const mirror = memoryStorage({ [LANG_STORAGE_KEY]: "pt-BR" });
    const server = fakeUiStateServer({ rev: 3, data: { [UI_SECTIONS.language]: { lang: "en" } } });
    const sync = makeGlobalSync(server);
    const off = bindLanguageToUiState(sync, { storage: mirror });
    expect(getLanguage()).toBe("pt-BR");
    await sync.start();
    expect(getLanguage()).toBe("en");
    expect(document.documentElement.lang).toBe("en");
    expect(mirror.getItem(LANG_STORAGE_KEY)).toBe("en");
    off();
    sync.stop();
  });

  it("antes da resposta do servidor nada muda", () => {
    const mirror = memoryStorage();
    const sync = {
      subscribe: () => () => undefined,
      getSnapshot: () => ({ rev: 0, data: { language: { lang: "en" } }, ready: false }),
      get: () => ({ lang: "en" }),
    };
    bindLanguageToUiState(sync, { storage: mirror });
    expect(getLanguage()).toBe("pt-BR");
    expect(mirror.getItem(LANG_STORAGE_KEY)).toBeNull();
  });

  it("sem preferência no servidor fica o idioma detectado e nada é gravado", async () => {
    const mirror = memoryStorage();
    const server = fakeUiStateServer({ rev: 1, data: {} });
    const sync = makeGlobalSync(server);
    const off = bindLanguageToUiState(sync, { storage: mirror });
    await sync.start();
    expect(getLanguage()).toBe("pt-BR");
    expect(mirror.getItem(LANG_STORAGE_KEY)).toBeNull();
    expect(server.calls.filter((c) => c.method === "PUT")).toHaveLength(0);
    off();
    sync.stop();
  });

  it("chooseLanguage vale na hora e vai para o servidor; outra aba acompanha", async () => {
    const mirror = memoryStorage();
    const hub = memoryBroadcastHub();
    const server = fakeUiStateServer({ rev: 0, data: {} });
    const mine = makeGlobalSync(server, hub);
    const other = makeGlobalSync(server, hub);
    await mine.start();
    await other.start();
    chooseLanguage("en", mine, { storage: mirror });
    expect(getLanguage()).toBe("en");
    expect(mirror.getItem(LANG_STORAGE_KEY)).toBe("en");
    await mine.flush();
    await settle();
    const put = server.calls.find((c) => c.method === "PUT");
    expect(put?.body).toMatchObject({ section: "language", value: { lang: "en" } });

    // Outra aba ligada ao mesmo servidor: a troca chega pelo canal.
    const otherMirror = memoryStorage();
    const off = bindLanguageToUiState(other, { storage: otherMirror });
    expect(otherMirror.getItem(LANG_STORAGE_KEY)).toBe("en");
    chooseLanguage("pt-BR", mine, { storage: mirror });
    await mine.flush();
    await settle();
    expect(other.get(UI_SECTIONS.language)).toEqual({ lang: "pt-BR" });
    expect(otherMirror.getItem(LANG_STORAGE_KEY)).toBe("pt-BR");
    off();
    mine.stop();
    other.stop();
  });
});
