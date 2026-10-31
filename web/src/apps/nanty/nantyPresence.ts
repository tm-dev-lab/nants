/*
 * Aba visível para o Nanty (spec 138 T07, C-07): com uma aba do Nants à vista, o servidor segura o
 * envio ao Telegram por alguns minutos, e o que a pessoa decidir na janela nesse meio não vai para
 * lá. A aba avisa no `visibilitychange` e, enquanto segue visível, renova o aviso a cada
 * `PRESENCE_HEARTBEAT_MS` (o servidor esquece um aviso sem renovação em 150 s). Ao sair (`pagehide`
 * ou desmontar), avisa que ficou oculta, com `keepalive`.
 *
 * Um sinal leve: `POST /api/nanty/presence {tab, visible}`, sem resposta a tratar. Falha de rede não
 * muda nada na tela; no pior caso o Telegram recebe sem esperar.
 */
import { request } from "../../api/client";

export const PRESENCE_PATH = "/api/nanty/presence";
/** Renovação do aviso enquanto a aba segue visível. */
const PRESENCE_HEARTBEAT_MS = 60_000;

type PresenceSend = (tab: string, visible: boolean, keepalive: boolean) => void;

interface PresenceOptions {
  doc?: Document;
  win?: Window;
  /** Testes: troca o envio. */
  send?: PresenceSend;
  heartbeatMs?: number;
  tabId?: string;
}

/** Id aleatório da aba (16 hex): o servidor conta cada aba à parte. */
function newTabId(): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

const defaultSend: PresenceSend = (tab, visible, keepalive) => {
  void request({ method: "POST", path: PRESENCE_PATH, body: JSON.stringify({ tab, visible }), keepalive });
};

/** Liga o aviso de aba visível; devolve o desinstalador (que avisa "oculta"). */
export function installNantyPresence(options: PresenceOptions = {}): () => void {
  const doc = options.doc ?? document;
  const win = options.win ?? window;
  const send = options.send ?? defaultSend;
  const heartbeat = options.heartbeatMs ?? PRESENCE_HEARTBEAT_MS;
  const tab = options.tabId ?? newTabId();
  let timer: number | null = null;

  const stopTimer = (): void => {
    if (timer !== null) win.clearInterval(timer);
    timer = null;
  };
  const sync = (): void => {
    const visible = doc.visibilityState === "visible";
    send(tab, visible, !visible);
    if (visible && timer === null) timer = win.setInterval(() => send(tab, true, false), heartbeat);
    if (!visible) stopTimer();
  };
  const onPageHide = (): void => {
    stopTimer();
    send(tab, false, true);
  };

  sync();
  doc.addEventListener("visibilitychange", sync);
  win.addEventListener("pagehide", onPageHide);
  return () => {
    doc.removeEventListener("visibilitychange", sync);
    win.removeEventListener("pagehide", onPageHide);
    stopTimer();
    send(tab, false, true);
  };
}
