/*
 * Conteúdo da barra superior (spec 093, decisão 8), desenhado no espaço reservado do Desktop
 * (o botão N no começo e Atalhos e Personalizar no fim ficam com o próprio Desktop): seletor de espaços, menu
 * Layout, pílula de execução, YOLO e Internet, selo "Setup incompleto", Buscar e o sino
 * "Precisa de você" com a contagem do `/api/attention`.
 *
 * Celular (`compact`, T10): só o espaço, a pílula, a busca (só o ícone, C-16), o sino e o selo de
 * setup (decisão 17).
 *
 * 095/T02 (`reference/Main.dc.html`, decisão 9): controles neutros do conjunto único de botões;
 * a busca é larga ("Buscar ou pedir" + o atalho em `<kbd>` sem serifa).
 *
 * 137/T03 (G2): a barra nunca quebra linha. Só a faixa de status (a pílula) encolhe; os outros
 * controles ficam com a largura deles e, onde a largura aperta, alguns trocam o rótulo pelo ícone
 * (faixas fixas em `shell.css`), com o nome no `aria-label` e na dica.
 */
import { memo } from "react";
import type { ProjectEntry } from "../../api/types";
import type { Attention } from "../api";
import { tp, t } from "../../i18n";
import { useT } from "../../i18n/react";
import { Icon } from "../Icon";
import { shortcutById } from "../shortcuts";
import type { SpaceData } from "../useSpaceData";
import { LayoutMenu, type LayoutCommand } from "./LayoutMenu";
import { InternetToggle, YoloToggle } from "./ProjectToggles";
import { RunPill } from "./RunPill";
import { SpaceSwitcher } from "./SpaceSwitcher";

/** Rótulo acessível do sino com a contagem (no idioma ativo). */
function bellLabel(count: number): string {
  if (count === 0) return t("topbar.bellNothing");
  return tp("topbar.bellCount", count);
}

interface TopBarProps {
  spaceId: string | null;
  space: SpaceData | null;
  projects: readonly ProjectEntry[];
  attention: Attention | null;
  attentionCount: number;
  setupIncomplete: boolean;
  /** Menu Layout (só com espaço). */
  layout?: { hasMine: boolean; onCommand: (command: LayoutCommand) => void };
  bellOpen: boolean;
  /** Modo celular (decisão 17): barra compacta com espaço, pílula, sino e o selo de setup. */
  compact?: boolean;
  stopSignal?: number;
  resumeSignal?: number;
  onSwitchSpace: (projectId: string) => void;
  onNewProject: () => void;
  onOpenSetup: () => void;
  onOpenLauncher: () => void;
  onToggleBell: () => void;
}

/**
 * Memo: com as props estáveis do shell, o streaming (atividades, mensagens) e os eventos de outros
 * projetos não re-renderizam a barra.
 */
export const TopBar = memo(function TopBar(props: TopBarProps) {
  const { space, attentionCount } = props;
  // Spec 101: a barra (memo) re-renderiza na troca de idioma.
  const { t: tr } = useT();
  const searchLabel = tr("topbar.search");
  const launcher = shortcutById("launcher");
  const state = space?.state ?? null;
  const setupBadge = props.setupIncomplete ? (
    <button
      type="button"
      className="nd-btn nd-bar-btn nd-setup-badge"
      data-setup-badge=""
      // Onde a barra aperta (137/T03), só o ícone aparece: o nome segue no rótulo e na dica.
      aria-label={tr("topbar.setupIncomplete")}
      title={tr("topbar.setupIncomplete")}
      onClick={props.onOpenSetup}
    >
      <Icon name="alert" size={14} className="nd-icon-warn" />
      <span className="nd-bar-label">{tr("topbar.setupIncomplete")}</span>
    </button>
  ) : null;
  const bell = (
    <button
      type="button"
      className="nd-btn nd-iconbtn nd-bell"
      data-bell=""
      aria-label={bellLabel(attentionCount)}
      aria-expanded={props.bellOpen}
      onClick={props.onToggleBell}
    >
      <Icon name="bell" size={16} />
      {attentionCount > 0 ? (
        <span className="nd-badge" data-badge="" aria-hidden="true">
          {attentionCount > 99 ? 99 : attentionCount}
        </span>
      ) : null}
    </button>
  );
  const search = (compact: boolean) => (
    <button
      type="button"
      // Compacta (celular): botão de ícone, com alvo de toque de 44 × 44 px (`.nd-iconbtn` no mobile.css).
      className={compact ? "nd-btn nd-iconbtn nd-bar-btn nd-search-btn" : "nd-btn nd-bar-btn nd-search-btn"}
      data-search=""
      data-compact={compact ? "true" : undefined}
      // Rótulo e dica sempre: no celular e onde a barra aperta (137/T03), só o ícone aparece.
      aria-label={searchLabel}
      title={searchLabel}
      aria-keyshortcuts={launcher?.aria}
      onClick={props.onOpenLauncher}
    >
      <Icon name="search" size={14} />
      {compact ? null : (
        <>
          <span className="nd-search-text">{searchLabel}</span>
          {launcher !== undefined ? <kbd>{launcher.keys}</kbd> : null}
        </>
      )}
    </button>
  );
  if (props.compact === true) {
    // Celular: 1ª linha com o espaço, o selo de setup e o sino; 2ª linha com a pílula.
    return (
      <div className="nd-topbar-content" data-topbar="" data-compact="true">
        <div className="nd-topbar-row">
          <SpaceSwitcher
            spaceId={props.spaceId}
            spaceName={space?.name ?? ""}
            spacePhase={space?.phase ?? null}
            projects={props.projects}
            attention={props.attention}
            onSwitch={props.onSwitchSpace}
            onNewProject={props.onNewProject}
          />
          {setupBadge}
          {search(true)}
          {bell}
        </div>
        {space !== null && state !== null ? (
          <div className="nd-topbar-row" data-topbar-pill="">
            <RunPill
              space={space}
              {...(props.stopSignal !== undefined ? { stopSignal: props.stopSignal } : {})}
              {...(props.resumeSignal !== undefined ? { resumeSignal: props.resumeSignal } : {})}
            />
          </div>
        ) : null}
      </div>
    );
  }
  return (
    <div className="nd-topbar-content" data-topbar="">
      <SpaceSwitcher
        spaceId={props.spaceId}
        spaceName={space?.name ?? ""}
        spacePhase={space?.phase ?? null}
        projects={props.projects}
        attention={props.attention}
        onSwitch={props.onSwitchSpace}
        onNewProject={props.onNewProject}
      />
      {props.layout !== undefined && space !== null ? (
        <LayoutMenu category={space.category} hasMine={props.layout.hasMine} onCommand={props.layout.onCommand} />
      ) : null}
      <div className="nd-topbar-center">
        {space !== null && state !== null ? (
          <RunPill
            space={space}
            {...(props.stopSignal !== undefined ? { stopSignal: props.stopSignal } : {})}
            {...(props.resumeSignal !== undefined ? { resumeSignal: props.resumeSignal } : {})}
          />
        ) : null}
      </div>
      {space !== null && state !== null ? (
        <>
          <YoloToggle projectId={space.projectId} state={state} onChanged={space.reload} />
          <InternetToggle projectId={space.projectId} state={state} />
        </>
      ) : null}
      {setupBadge}
      {search(false)}
      {bell}
    </div>
  );
});
