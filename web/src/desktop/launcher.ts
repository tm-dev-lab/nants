/*
 * Modelo do lançador (spec 093, decisão 11): itens de janelas, projetos e ações, busca sem
 * acento nem caixa, navegação por setas. Funções puras (o componente fica em `Launcher.tsx`).
 */
import { localized } from "../i18n";
import type { IconName } from "./Icon";

export type LauncherGroup = "windows" | "projects" | "actions";

export const LAUNCHER_GROUP_LABELS: Readonly<Record<LauncherGroup, string>> = localized({
  windows: "launcher.group.windows",
  projects: "launcher.group.projects",
  actions: "launcher.group.actions",
});

const GROUP_ORDER: readonly LauncherGroup[] = ["actions", "windows", "projects"];

export interface LauncherItem {
  id: string;
  group: LauncherGroup;
  label: string;
  icon: IconName;
  /** Atalho exibido (da tabela única), por exemplo "Alt+2". */
  keys?: string;
  /** `aria-keyshortcuts` correspondente. */
  aria?: string;
  /** Texto curto à direita (estado da janela, fase do projeto). */
  hint?: string;
  /** Sempre visível (não é filtrado pela busca), como "Pedir ao Arquiteto". */
  always?: boolean;
  /** Item visível mas indisponível agora, com o motivo (mostrado no lugar da dica). Enter não faz nada. */
  disabled?: string;
  run: () => void;
}

/** Texto para comparação: minúsculas e sem acentos. */
export function normalizeQuery(text: string): string {
  return text.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLowerCase().trim();
}

/** Itens que casam com a busca (todas as palavras), na ordem dos grupos. */
export function filterLauncher(items: readonly LauncherItem[], query: string): LauncherItem[] {
  const words = normalizeQuery(query).split(/\s+/).filter((w) => w !== "");
  const matches = items.filter((item) => {
    if (item.always === true) return true;
    if (words.length === 0) return true;
    const text = normalizeQuery(`${item.label} ${item.hint ?? ""}`);
    return words.every((w) => text.includes(w));
  });
  // Os itens "always" (pedir ao Arquiteto) vão para o fim: o destaque inicial fica no que casou.
  const found = GROUP_ORDER.flatMap((group) => matches.filter((item) => item.group === group && item.always !== true));
  return [...found, ...matches.filter((item) => item.always === true)];
}

/** Próximo índice ativo (circular); sem itens, -1. */
export function moveActive(current: number, delta: number, length: number): number {
  if (length <= 0) return -1;
  if (current < 0) return delta > 0 ? 0 : length - 1;
  return (current + delta + length) % length;
}
