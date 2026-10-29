/*
 * Metadados de cada tipo de janela (spec 093, decisão 3): título, ícone, tom do ícone e o estado
 * vazio padrão (uma frase e uma ação). Também a ordem do dock.
 * Spec 101: título e frases vêm do dicionário, lidos no idioma ativo a cada acesso (quem desenha
 * assina o idioma com `useT()`/`useLanguage()`).
 */
import { t, type MessageKey } from "../i18n";
import type { IconName } from "./Icon";
import type { Tone } from "./theme/tokens";
import type { WindowKind } from "./windows";

interface WindowEmptyDefault {
  readonly message: string;
  /** Ação padrão: abrir outra janela. */
  action?: { readonly label: string; open: WindowKind };
}

interface WindowMeta {
  readonly title: string;
  icon: IconName;
  tone: Tone;
  empty: WindowEmptyDefault;
}

/** Rótulo de uma ação do estado vazio (lido no idioma ativo). */
function emptyAction(label: MessageKey, open: WindowKind): { readonly label: string; open: WindowKind } {
  return {
    get label() {
      return t(label);
    },
    open,
  };
}

/** Metadados de um tipo; título e frases são lidos no idioma ativo a cada acesso. */
function meta(title: MessageKey, icon: IconName, tone: Tone, message: MessageKey, action?: { label: MessageKey; open: WindowKind }): WindowMeta {
  const empty: WindowEmptyDefault = {
    get message() {
      return t(message);
    },
    ...(action !== undefined ? { action: emptyAction(action.label, action.open) } : {}),
  };
  return {
    get title() {
      return t(title);
    },
    icon,
    tone,
    empty,
  };
}

export const WINDOW_META: Readonly<Record<WindowKind, WindowMeta>> = {
  conversation: meta("window.conversation", "conversation", "violet", "windowEmpty.conversation"),
  plan: meta("window.plan", "plan", "violet", "windowEmpty.plan", { label: "windowEmpty.openConversation", open: "conversation" }),
  board: meta("window.board", "board", "cyan", "windowEmpty.board", { label: "windowEmpty.openPlan", open: "plan" }),
  activity: meta("window.activity", "activity", "amber", "windowEmpty.activity"),
  files: meta("window.files", "files", "violet", "windowEmpty.files"),
  app: meta("window.app", "app", "green", "windowEmpty.app", { label: "windowEmpty.openBoard", open: "board" }),
  execution: meta("window.run", "execution", "cyan", "windowEmpty.run", { label: "windowEmpty.openBoard", open: "board" }),
  costs: meta("window.costs", "costs", "amber", "windowEmpty.costs"),
  report: meta("window.report", "report", "green", "windowEmpty.report", { label: "windowEmpty.openBoard", open: "board" }),
  knowledge: meta("window.knowledge", "knowledge", "cyan", "windowEmpty.knowledge"),
  settings: meta("window.settings", "settings", "muted", "windowEmpty.settings"),
  project: meta("window.project", "project", "cyan", "windowEmpty.project"),
  setup: meta("window.gettingStarted", "setup", "amber", "windowEmpty.gettingStarted"),
};

/**
 * Ordem do dock (095/T03, C-04): Alt+1…9 cobre as nove primeiras (até Custos); Conhecimento
 * (spec 134 T06), Projeto, Configurações e Primeiros passos ficam pelo lançador (sem Alt+0). Primeiros passos entra no
 * fim, quando aberto.
 */
export const DOCK_ORDER: readonly WindowKind[] = [
  "conversation",
  "plan",
  "board",
  "activity",
  "files",
  "app",
  "execution",
  "report",
  "costs",
  "knowledge",
  "project",
  "settings",
  "setup",
];

/** Id do subtítulo vivo da janela (`aria-describedby`). */
export const windowSubtitleId = (kind: WindowKind): string => `window-${kind}-subtitle`;
