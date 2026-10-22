/*
 * Contextos do shell (spec 093, T05), consumidos pelas janelas das tarefas T06 a T09:
 * - `ShellContext` (`useShell()`): espaço ativo, abrir janelas e diálogos, atenção, prontidão e
 *   avisos. Existe com ou sem espaço.
 * - `SpaceContext` (`useSpace()`): os dados do espaço ativo (`useSpaceData`, carregados uma vez
 *   pelo shell), a seleção salva no ui-state (`selection`) e o pedido ao Arquiteto vindo do
 *   lançador. `null` nas boas-vindas.
 *
 * O que muda a cada evento do streaming NÃO está no contexto (re-renderizaria todas as janelas):
 * cada janela lê a própria fatia por seletor do store, com o `projectId` de `useSpace()`:
 * - mensagens: `useSpaceMessages(projectId)`;
 * - atividades ao vivo: `useSpaceActivities(projectId)`;
 * - permissões resolvidas: `useSpaceResolvedPermissions(projectId)`;
 * - outras fatias: `useAppStore((s) => s.<mapa>[projectId] ?? CONSTANTE_VAZIA)`, sempre com
 *   uma constante vazia estável (nunca `[]` literal no seletor).
 * Todos ficam em `useSpaceData.ts`.
 */
import { createContext, useContext } from "react";
import type { LanRange } from "../api/types";
import type { Attention, Readiness, SettingsWindowTab } from "./api";
import type { UiSelection } from "./uiState";
import type { SpaceData } from "./useSpaceData";
import type { WindowKind } from "./windows";

export interface ToastOptions {
  /** Mostra "Desfazer" (a última mudança de layout) enquanto ela puder ser desfeita. */
  undo?: boolean;
  /**
   * Aviso com título (095/T03, "App no ar"): ícone de confirmação, título em 600 e a mensagem
   * abaixo, em `--muted`. A entrada sobe e aparece; sob `prefers-reduced-motion`, só aparece.
   */
  title?: string;
}

export interface ShellApi {
  /** Espaço ativo (`null` nas boas-vindas). */
  spaceId: string | null;
  /** Troca de espaço (`null` volta às boas-vindas); a URL acompanha. */
  switchSpace: (projectId: string | null) => void;
  /** Abre (ou foca) uma janela do espaço, com foco e anúncio. */
  openWindow: (kind: WindowKind) => void;
  openNewProject: () => void;
  openSetup: () => void;
  openLauncher: () => void;
  attention: Attention | null;
  /** Itens do sino (tudo o que exige ação, de todos os projetos). */
  attentionCount: number;
  refreshAttention: () => void;
  readiness: Readiness | null;
  /** Relê a prontidão; devolve a resposta nova (ou `null` se a leitura falhou). */
  reloadReadiness: () => Promise<Readiness | null>;
  recheckMachine: () => Promise<boolean>;
  /** Aviso flutuante (`role="status"`) por 8 s. */
  showToast: (message: string, options?: ToastOptions) => void;
  /**
   * Abre uma janela de um projeto (T06): no espaço ativo, abre e foca; em outro, troca de espaço
   * e abre a janela quando o novo espaço estiver pronto ("Ir para a conversa", "Abrir Plano").
   */
  goTo: (projectId: string, kind: WindowKind) => void;
  /** Abre as Configurações (T08) já na aba pedida ("Configurar" de uma pendência de prontidão). */
  openSettings: (tab?: SettingsWindowTab) => void;
  /** Aba pedida pelo último `openSettings`; a janela Configurações (T08) a aplica e a consome. */
  settingsRequest: SettingsRequest | null;
  consumeSettingsRequest: (id: number) => void;
  /**
   * Faixa de portas de visualização marcada como liberada na rede (spec 097, "Já liberei"), salva
   * no estado da interface global (vale para todos os aparelhos); `null` = nenhuma.
   */
  lanReleased: LanRange | null;
  /** Marca (faixa) ou desmarca (`null`) a liberação. */
  setLanReleased: (range: LanRange | null) => void;
}

/** Pedido de abertura das Configurações numa aba (`openSettings`). */
export interface SettingsRequest {
  id: number;
  tab: SettingsWindowTab;
}

const noop = (): void => undefined;

export const DEFAULT_SHELL: ShellApi = {
  spaceId: null,
  switchSpace: noop,
  openWindow: noop,
  openNewProject: noop,
  openSetup: noop,
  openLauncher: noop,
  attention: null,
  attentionCount: 0,
  refreshAttention: noop,
  readiness: null,
  reloadReadiness: async () => null,
  recheckMachine: async () => false,
  showToast: noop,
  goTo: noop,
  openSettings: noop,
  settingsRequest: null,
  consumeSettingsRequest: noop,
  lanReleased: null,
  setLanReleased: noop,
};

export const ShellContext = createContext<ShellApi>(DEFAULT_SHELL);

export function useShell(): ShellApi {
  return useContext(ShellContext);
}

/** Pedido ao Arquiteto feito pelo lançador; a Conversa (T06) o põe no campo e o consome. */
export interface ArchitectRequest {
  id: number;
  text: string;
}

export interface SpaceContextValue {
  data: SpaceData;
  selection: UiSelection;
  /** Grava parte da seleção (tarefa em foco, arquivo aberto, rolagem da Conversa). */
  setSelection: (patch: Partial<UiSelection>) => void;
  architectRequest: ArchitectRequest | null;
  consumeArchitectRequest: (id: number) => void;
}

export const SpaceContext = createContext<SpaceContextValue | null>(null);

/** Espaço ativo; `null` nas boas-vindas. */
export function useSpace(): SpaceContextValue | null {
  return useContext(SpaceContext);
}
