/*
 * Regras puras da janela App (spec 093, decisão 12):
 * fase da execução a partir do estado do servidor, primeiro link publicado, guardas do iframe,
 * tamanhos da tela com a escala para caber e o resumo da janela.
 *
 * Todo projeto do Nants é Docker (spec 074; o modo processo saiu na T02): o app sobe o Compose
 * `-run` do projeto, e os links são as portas publicadas, com o MESMO host da página.
 */
import type { AppPort, AppStatus } from "../../api/client";
import type { LanRange } from "../../api/types";
import { rangeLabel } from "../../desktop/network";
import type { IconName } from "../../desktop/Icon";
import type { AppViewport } from "../../desktop/uiState";
import { localized, t, type MessageKey } from "../../i18n";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Intervalo da consulta do console enquanto o app está no ar (e a janela visível). */
export const LOGS_POLL_MS = 2_000;

/** Comando exato mostrado na confirmação (o servidor sobe o Compose `-run` do projeto). */
export const DOCKER_RUN_COMMAND = "docker compose up -d --build --wait";

/** `sandbox` do iframe do app (decisão 12). */
export const IFRAME_SANDBOX = "allow-scripts allow-forms allow-same-origin allow-popups allow-popups-to-escape-sandbox";

export type RunPhase =
  | { kind: "stopped" }
  | { kind: "starting" }
  | { kind: "running"; ports: AppPort[]; embeddable: boolean | null }
  | { kind: "other" }
  | { kind: "exited"; code: number | null; reason: string | null };

/**
 * Fase a partir do `GET .../app`. Com `specId` (a especificação concluída do espaço), um app de
 * outra especificação no ar vira `other`; sem `specId`, o app no ar do projeto é mostrado.
 */
export function phaseFromStatus(status: AppStatus, specId: string | null): RunPhase {
  if (status.state === "running") {
    const mine = specId === null || status.spec_id === null || status.spec_id === specId;
    return mine ? { kind: "running", ports: status.ports, embeddable: status.embeddable } : { kind: "other" };
  }
  if (status.state === "exited" && specId !== null && status.spec_id === specId) {
    return { kind: "exited", code: status.exit_code, reason: null };
  }
  return { kind: "stopped" };
}

/**
 * Estado relido do servidor aplicado à fase atual: na carga inicial (fase parada) adota o do
 * servidor; com o app já no ar, completa as portas e o `embeddable`, e se a releitura mostra o
 * app parado ou encerrado (o `app_exited` se perdeu), adota o do servidor em vez de ficar preso
 * em "No ar". Partida em curso ou encerramento já conhecido não são desfeitos por uma resposta
 * antiga.
 */
export function mergeStatus(current: RunPhase, next: RunPhase): RunPhase {
  if (current.kind === "stopped") return next;
  if (current.kind === "running" && (next.kind === "stopped" || next.kind === "exited")) return next;
  if (current.kind === "running" && next.kind === "running") {
    const ports = next.ports.length > 0 ? next.ports : current.ports;
    const embeddable = next.ports.length > 0 ? next.embeddable : current.embeddable;
    if (ports === current.ports && embeddable === current.embeddable) return current;
    return { kind: "running", ports, embeddable };
  }
  return current;
}

/**
 * Ordem das leituras do estado do app. Cada GET leva o número do momento em que saiu (`issue`);
 * uma mudança local da execução (partida pedida, resposta do POST, `app_started`/`app_exited`)
 * avança a marca (`markLocal`). `accept` só aceita a resposta que saiu depois da última mudança
 * local e depois da última resposta aplicada: um GET lento da carga inicial ("parado") que chega
 * depois do POST de partida não derruba o "No ar".
 */
interface StatusSequence {
  issue: () => number;
  markLocal: () => void;
  accept: (issued: number) => boolean;
}

export function createStatusSequence(): StatusSequence {
  let counter = 0;
  let localMark = 0;
  let applied = 0;
  return {
    issue: () => {
      counter += 1;
      return counter;
    },
    markLocal: () => {
      counter += 1;
      localMark = counter;
    },
    accept: (issued) => {
      if (issued <= localMark || issued <= applied) return false;
      applied = issued;
      return true;
    },
  };
}

/** Primeira porta publicada com link (o que o iframe mostra), ou `null`. */
export function firstPort(phase: RunPhase): AppPort | null {
  return phase.kind === "running" ? (phase.ports[0] ?? null) : null;
}

/**
 * Porta que o App mostra (095/T07, portas na Execução): a escolhida em "Abrir no App" (pela URL),
 * se ainda está publicada; senão a primeira.
 */
export function shownPort(phase: RunPhase, chosenUrl: string | null): AppPort | null {
  if (phase.kind !== "running") return null;
  const chosen = chosenUrl === null ? undefined : phase.ports.find((p) => p.url === chosenUrl);
  return chosen ?? firstPort(phase);
}

/** `embeddable` da porta mostrada (o da porta; sem veredito dela, o do app). */
export function portEmbeddable(phase: RunPhase, port: AppPort): boolean | null {
  if (port.embeddable !== null) return port.embeddable;
  return phase.kind === "running" ? phase.embeddable : null;
}

/** Rótulo curto `host:porta` de uma URL (a própria URL se não for analisável). */
export function hostPort(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

// --- Guardas do iframe ---

type EmbedBlock = "invalid" | "not_http" | "other_host" | "same_origin" | "not_embeddable";

type EmbedDecision = { embed: true; confirmed: boolean } | { embed: false; reason: EmbedBlock };

interface PageLocation {
  hostname: string;
  origin: string;
}

/**
 * Só embute se TODAS valerem: `http:`, host igual ao da página, origem diferente da do Nants e
 * `embeddable !== false`. Com `embeddable === null` tenta embutir, sem confirmação.
 */
export function embedDecision(url: string, embeddable: boolean | null, page: PageLocation): EmbedDecision {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return { embed: false, reason: "invalid" };
  }
  if (parsed.protocol !== "http:") return { embed: false, reason: "not_http" };
  if (parsed.username !== "" || parsed.password !== "") return { embed: false, reason: "invalid" };
  if (parsed.hostname !== page.hostname) return { embed: false, reason: "other_host" };
  if (parsed.origin === page.origin) return { embed: false, reason: "same_origin" };
  if (embeddable === false) return { embed: false, reason: "not_embeddable" };
  return { embed: true, confirmed: embeddable === true };
}

/** Motivo de cada guarda do iframe, no idioma ativo. */
export const EMBED_BLOCK_MESSAGES: Readonly<Record<EmbedBlock, string>> = localized({
  invalid: "app.embed.invalid",
  not_http: "app.embed.not_http",
  other_host: "app.embed.other_host",
  same_origin: "app.embed.same_origin",
  not_embeddable: "app.embed.not_embeddable",
});

/**
 * Faixa acima do iframe sem veredito `embeddable`, no idioma ativo: com `probing`, o app ainda não
 * respondeu à conferência do servidor (o estado é relido); senão, a explicação da área em branco.
 */
export function embedNote(probing: boolean): string {
  return t(probing ? "app.embed.probing" : "app.embed.unconfirmed");
}

/**
 * Releitura do estado enquanto o veredito do `embeddable` é indeterminado (spec 094, T05): no
 * Test 11 o iframe abriu antes de o servidor do app ouvir (o `up --wait` volta com o contêiner no
 * ar, mas o `npm ci` e o servidor de desenvolvimento ainda sobem) e, no iPhone, a página que
 * falhou fica em branco para sempre. O servidor sonda a porta a cada leitura; quando o app
 * responde, o iframe é recarregado uma vez. Teto: `EMBED_RETRY_LIMIT` leituras (3 min).
 */
export const EMBED_RETRY_MS = 5_000;
export const EMBED_RETRY_LIMIT = 36;

/** O iframe mostrado precisa da conferência de novo: no ar, com porta e veredito indeterminado. */
export function embedProbePending(phase: RunPhase): boolean {
  const port = firstPort(phase);
  return port !== null && portEmbeddable(phase, port) === null;
}

/**
 * O app passou a responder depois de o iframe ter sido aberto sem resposta: recarrega uma vez
 * (a página que falhou não volta sozinha).
 */
export function reloadOnEmbedVerdict(previous: boolean | null | undefined, next: boolean | null | undefined): boolean {
  return previous === null && next === true;
}

/** O link de "Abrir em nova aba" só existe para `http:`/`https:` (nunca `javascript:` e afins). */
export function safeHref(url: string): string | null {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:" ? url : null;
  } catch {
    return null;
  }
}

// --- Tamanhos da tela ---

interface ViewportOption {
  id: AppViewport;
  /** Rótulo no idioma ativo (lido na hora). */
  readonly label: string;
  /** Largura simulada em px; `null` = livre (a área inteira da janela). */
  width: number | null;
  icon: IconName;
}

function viewportOption(id: AppViewport, key: MessageKey, width: number | null, icon: IconName): ViewportOption {
  return {
    id,
    get label() {
      return t(key);
    },
    width,
    icon,
  };
}

export const APP_VIEWPORTS: readonly ViewportOption[] = [
  viewportOption("free", "app.viewport.free", null, "expand"),
  viewportOption("desktop", "app.viewport.desktop", 1440, "monitor"),
  viewportOption("tablet", "app.viewport.tablet", 768, "tablet"),
  viewportOption("phone", "app.viewport.phone", 390, "smartphone"),
];

function viewportWidth(viewport: AppViewport): number | null {
  return APP_VIEWPORTS.find((v) => v.id === viewport)?.width ?? null;
}

interface FrameGeometry {
  /** Largura do iframe em px; `null` = 100% da área. */
  width: number | null;
  /** Escala por `transform` para caber na largura da área (1 = sem escala). */
  scale: number;
}

/**
 * Geometria do iframe: livre ocupa a área; um tamanho fixo mais largo que a área é reduzido por
 * `transform: scale` para caber (a altura é compensada para preencher a área). Área ainda não
 * medida (0): sem escala.
 */
export function frameGeometry(viewport: AppViewport, areaWidth: number): FrameGeometry {
  const width = viewportWidth(viewport);
  if (width === null) return { width: null, scale: 1 };
  if (areaWidth <= 0 || width <= areaWidth) return { width, scale: 1 };
  return { width, scale: Math.round((areaWidth / width) * 10_000) / 10_000 };
}

/** Estilo do iframe para a geometria. */
export function frameStyle(geometry: FrameGeometry): { width: string; height: string; transform?: string; transformOrigin?: string } {
  if (geometry.width === null) return { width: "100%", height: "100%" };
  if (geometry.scale === 1) return { width: `${geometry.width}px`, height: "100%" };
  return {
    width: `${geometry.width}px`,
    height: `${Math.round((100 / geometry.scale) * 100) / 100}%`,
    transform: `scale(${geometry.scale})`,
    transformOrigin: "0 0",
  };
}

// --- Estados vazios do App (095/T03, C-10) ---

/**
 * O que a área do App mostra quando não há página: um estado vazio por situação, cada um com
 * título, uma frase, uma ação primária e "Abrir Execução". A ordem decide: porta com endereço
 * (página ou embed bloqueado) → no ar sem endereço → subindo (inclui a confirmação aberta) →
 * outra especificação → encerrado → sem especificação concluída → bloqueado pela prontidão →
 * parado. `null` = há página para mostrar (o iframe).
 */
export type AppEmptyKind = "stopped" | "starting" | "no_spec" | "blocked" | "other" | "exited" | "embed_blocked" | "no_address";

interface AppEmptyInput {
  phase: RunPhase;
  confirming: boolean;
  specId: string | null;
  /** A prontidão `execute` está incompleta. */
  blocked: boolean;
  /** Decisão do iframe para a porta mostrada (`null` sem porta com endereço). */
  embed: EmbedDecision | null;
}

export function appEmptyKind({ phase, confirming, specId, blocked, embed }: AppEmptyInput): AppEmptyKind | null {
  if (embed !== null) return embed.embed ? null : "embed_blocked";
  if (phase.kind === "running") return "no_address";
  if (phase.kind === "starting" || confirming) return "starting";
  if (phase.kind === "other") return "other";
  if (phase.kind === "exited") return "exited";
  if (specId === null) return "no_spec";
  if (blocked) return "blocked";
  return "stopped";
}

/** Títulos dos estados vazios (17 px, 600), no idioma ativo. */
export const APP_EMPTY_TITLES: Readonly<Record<AppEmptyKind, string>> = localized({
  stopped: "app.empty.stopped",
  starting: "app.empty.starting",
  no_spec: "app.empty.no_spec",
  blocked: "app.empty.blocked",
  other: "app.empty.other",
  exited: "app.empty.exited",
  embed_blocked: "app.empty.embed_blocked",
  no_address: "app.empty.no_address",
});

// --- Porta talvez não liberada na rede (095/T09; faixa fixa na 097) ---

/*
 * No celular, pela rede, a porta do app pode não chegar ao aparelho (o Nants no WSL: o Windows só
 * encaminha a porta do Nants) e o iframe fica branco. O navegador NÃO consegue sondar isso: no
 * Chrome, `fetch` no-cors para a rede local falha sempre (Local Network Access e ORB), alcançável
 * ou não, e a navegação do iframe não sofre esse bloqueio. Por isso o App só mostra uma ajuda
 * discreta, sem bloquear nada, quando a página do Nants foi aberta por um host que não é loopback.
 * Com a spec 097, em `--lan`, o app publica sempre numa faixa fixa de portas de visualização: a
 * ajuda fala da faixa e de uma liberação única, e "Já liberei" a silencia enquanto a faixa for a
 * mesma. Sem comandos nem detalhes de sistema (eles ficam em Configurações, só no WSL).
 */

/** A página do Nants foi aberta neste mesmo computador (loopback): a ajuda não aparece. */
export function isLoopbackHost(hostname: string): boolean {
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  return h === "localhost" || h.endsWith(".localhost") || h === "::1" || /^127(?:\.\d{1,3}){3}$/.test(h);
}

/** Porta numérica de uma URL do app (`null` se não analisável). */
export function urlPort(url: string): string | null {
  try {
    const parsed = new URL(url);
    return parsed.port !== "" ? parsed.port : parsed.protocol === "https:" ? "443" : "80";
  } catch {
    return null;
  }
}

/** Linha de ajuda do App (abaixo do navegador) com a faixa real (spec 097, decisão 5). */
export function reachRangeHint(range: LanRange): string {
  return t("app.reach.rangeHint", { range: rangeLabel(range) });
}

/** Ajuda curta da Execução, na linha da porta, com a faixa real. */
export function reachRangeNote(range: LanRange): string {
  return t("app.reach.rangeNote", { range: rangeLabel(range) });
}

/** Junto do "Já liberei": a marca é só uma declaração de quem usa. */
export function reachUnverifiedNote(): string {
  return t("app.reach.unverified");
}

/** Servidor sem a faixa (antigo): a ajuda genérica da 095/T09, pela porta. */
export function reachHintText(port: string): string {
  return t("app.reach.portHint", { port });
}

/** Ajuda curta da Execução, na linha da porta. */
export function reachPortNote(port: string): string {
  return t("app.reach.portNote", { port });
}

/** Aviso "App no ar" (095/T03): título e frase do aviso flutuante, no idioma ativo. */
export function showAppLiveToast(showToast: (text: string, options: { title: string }) => void): void {
  showToast(t("app.live.text"), { title: t("app.live.title") });
}

const RUN_STATE_KEYS: Readonly<Record<RunPhase["kind"], MessageKey>> = {
  stopped: "app.state.stopped",
  starting: "app.state.starting",
  running: "app.state.running",
  other: "app.state.other",
  exited: "app.state.exited",
};

/** Pílula de estado da Execução. */
export function runStateLabel(phase: RunPhase): string {
  return t(RUN_STATE_KEYS[phase.kind]);
}

// --- Resumo ---

/** Subtítulo da janela: a especificação concluída (pronta para testar) ou a espera. */
export function appSummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  const spec = space.activeSpec;
  if (spec === null || spec.status !== "COMPLETED") return { subtitle: t("summary.waitingCompletion") };
  return { subtitle: t("app.subtitle.ready", { spec: spec.id }) };
}
