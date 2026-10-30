/*
 * Registro das janelas (apps) do desktop. Cada tarefa (T06 a T09) acrescenta aqui o app dela:
 * - `Content`: o conteúdo da janela. Recebe o tamanho do contêiner e lê os dados pelos contextos
 *   (`useSpace()`, `useShell()`); nunca dispara a carga do espaço (isso é do shell).
 * - `summarize`: o subtítulo vivo, o selo do app (menu do N e celular) e o estado vazio, a partir dos dados do
 *   espaço (função pura, recalculada pelo shell quando os dados mudam).
 * - `scroll`: quem rola o conteúdo (spec 094, decisão 4). `"app"`: o app tem a própria área de
 *   rolagem, marcada com a classe `nd-app-scroll` (exatamente uma), e o conteúdo da janela não
 *   rola (regra única `.nd-content[data-scroll="app"]`); `"window"`: o app só flui e quem rola é
 *   o conteúdo da janela. Assim cada janela tem uma barra de rolagem só.
 * Sem `Content`, a janela mostra o estado vazio do tipo.
 */
import { createElement, memo, type ComponentType } from "react";
import type { AttentionProject, Readiness } from "../desktop/api";
import type { AppBadge } from "../desktop/appState";
import type { ContainerSize } from "../desktop/useContainerSize";
import type { SpaceData } from "../desktop/useSpaceData";
import type { WindowEmpty, WindowScroll } from "../desktop/Window";
import type { WindowKind } from "../desktop/windows";
import { ActivityContent } from "./activity/ActivityApp";
import { AppContent } from "./app/AppApp";
import { appSummary } from "./app/appModel";
import { activitySummary } from "./activity/activityModel";
import { BoardContent } from "./board/BoardApp";
import { boardSummary } from "./board/boardModel";
import { CostsContent } from "./costs/CostsApp";
import { costsSummary } from "./costs/costsModel";
import { ConversationContent } from "./conversation/ConversationApp";
import { conversationSummary } from "./conversation/summary";
import { PlanContent } from "./plan/PlanApp";
import { planSummary } from "./plan/summary";
import { ReportContent } from "./report/ReportApp";
import { reportSummary } from "./report/summary";
import { ExecutionContent } from "./execution/ExecutionApp";
import { executionSummary } from "./execution/executionModel";
import { FilesContent } from "./files/FilesApp";
import { KnowledgeContent } from "./knowledge/KnowledgeApp";
import { kbSummaryText } from "./knowledge/knowledgeModel";
import type { KbState } from "./knowledge/kbStore";
import { filesSummary } from "./files/filesModel";
import { ProjectContent } from "./project/ProjectApp";
import { projectSummary } from "./project/projectModel";
import { SettingsContent } from "./settings/SettingsApp";
import { settingsSummary } from "./settings/settingsModel";
import { SetupApp } from "./setup/SetupApp";
import { setupSubtitle } from "./setup/setup";

export interface AppContentProps {
  size: ContainerSize;
}

/** O que o `summarize` recebe: o espaço ativo (`null` nas boas-vindas) e os dados globais. */
export interface AppSummaryInput {
  space: SpaceData | null;
  readiness: Readiness | null;
  projectCount: number;
  /** Entrada do espaço ativo no `/api/attention` (T06), ou `null`. */
  attention?: AttentionProject | null;
  /** Mensagens novas da Conversa chegadas com a janela fora de vista (T06). */
  conversationUnseen?: number;
  /** Lista da base de conhecimento do espaço ativo (spec 134 T06), para o subtítulo vivo. */
  knowledge?: KbState | null;
}

export interface AppSummary {
  subtitle?: string;
  badge?: AppBadge;
  empty?: WindowEmpty;
}

interface AppDefinition {
  Content?: ComponentType<AppContentProps>;
  summarize?: (input: AppSummaryInput) => AppSummary;
  /** Quem rola o conteúdo da janela: o próprio app (`nd-app-scroll`) ou a janela. */
  scroll: WindowScroll;
}

export type AppRegistry = Partial<Record<WindowKind, AppDefinition>>;

/** Memo: a moldura chama o conteúdo a cada render dela; com o mesmo tamanho, o app não re-renderiza. */
const SetupContent = memo(function SetupContent() {
  return createElement(SetupApp);
});

export const DEFAULT_APPS: AppRegistry = {
  conversation: { Content: ConversationContent, summarize: conversationSummary, scroll: "app" },
  plan: { Content: PlanContent, summarize: planSummary, scroll: "app" },
  board: { Content: BoardContent, summarize: boardSummary, scroll: "app" },
  activity: { Content: ActivityContent, summarize: activitySummary, scroll: "app" },
  costs: { Content: CostsContent, summarize: costsSummary, scroll: "app" },
  report: { Content: ReportContent, summarize: reportSummary, scroll: "app" },
  files: { Content: FilesContent, summarize: filesSummary, scroll: "app" },
  app: { Content: AppContent, summarize: appSummary, scroll: "app" },
  knowledge: {
    Content: KnowledgeContent,
    scroll: "app",
    summarize: ({ space, knowledge }) => {
      const subtitle = space === null ? undefined : kbSummaryText(knowledge ?? null);
      return subtitle === undefined ? {} : { subtitle };
    },
  },
  execution: { Content: ExecutionContent, summarize: executionSummary, scroll: "app" },
  settings: { Content: SettingsContent, summarize: settingsSummary, scroll: "app" },
  project: { Content: ProjectContent, summarize: projectSummary, scroll: "app" },
  setup: {
    Content: SetupContent,
    scroll: "window",
    summarize: ({ readiness, projectCount }) => ({ subtitle: setupSubtitle(readiness, projectCount) }),
  },
};
