/*
 * Seletor de espaços (spec 093, decisões 2 e 8): o projeto ativo com a fase, a lista de projetos
 * com o estado de cada um (fase ou "precisa de você") e "Novo projeto".
 */
import { useMemo, useRef, useState } from "react";
import type { ProjectEntry } from "../../api/types";
import { PHASE_LABELS, phaseForProject } from "../../phase";
import { useAppStore, type ProjectPhase } from "../../store";
import { projectAttentionCount, type Attention } from "../api";
import { useT } from "../../i18n/react";
import { Icon } from "../Icon";
import { PopupMenu } from "../PopupMenu";
import { PHASE_TONE } from "../theme/tokens";

interface SpaceSwitcherProps {
  spaceId: string | null;
  /** Nome do espaço ativo (pode vir do estado antes da config). */
  spaceName: string;
  /** Fase do espaço ativo (a bolinha do botão); `null` sem espaço. */
  spacePhase: ProjectPhase | null;
  projects: readonly ProjectEntry[];
  attention: Attention | null;
  onSwitch: (projectId: string) => void;
  onNewProject: () => void;
}

/** Fase exibida de cada projeto conhecido (a mesma regra da barra). */
function useProjectPhases(projects: readonly ProjectEntry[]): Record<string, ProjectPhase> {
  const phases = useAppStore((s) => s.phases);
  const states = useAppStore((s) => s.projectStates);
  const turns = useAppStore((s) => s.architectTurns);
  return useMemo(() => {
    const out: Record<string, ProjectPhase> = {};
    for (const p of projects) out[p.id] = phaseForProject(phases[p.id], states[p.id] ?? null, turns[p.id] !== undefined);
    return out;
  }, [projects, phases, states, turns]);
}

export function SpaceSwitcher({ spaceId, spaceName, spacePhase, projects, attention, onSwitch, onNewProject }: SpaceSwitcherProps) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const current = spaceId === null ? null : (spacePhase ?? "idle");
  const label = spaceId === null ? t("space.none") : spaceName !== "" ? spaceName : spaceId;
  const close = (returnFocus: boolean): void => {
    setOpen(false);
    if (returnFocus) buttonRef.current?.focus({ preventScroll: true });
  };
  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className="nd-btn nd-bar-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={t("space.switch", { name: label })}
        data-space-switcher=""
        onClick={() => setOpen((v) => !v)}
      >
        {current !== null ? <span className="nd-dot nd-dot-sm" data-tone={PHASE_TONE[current]} aria-hidden="true" /> : null}
        <span className="nd-bar-text">{label}</span>
        <Icon name="chevron-down" size={14} />
      </button>
      {open ? (
        <PopupMenu label={t("common.projects")} anchor={buttonRef} onClose={close} width={320}>
          <SpaceMenuItems spaceId={spaceId} projects={projects} attention={attention} onPick={(id) => {
            close(false);
            onSwitch(id);
          }} />
          <button
            type="button"
            role="menuitem"
            className="nd-menu-item"
            data-new-project=""
            onClick={() => {
              close(false);
              onNewProject();
            }}
          >
            <Icon name="plus" size={16} />
            <span className="nd-menu-text">{t("common.newProject")}</span>
          </button>
        </PopupMenu>
      ) : null}
    </>
  );
}

/**
 * Itens do menu: a fase de cada projeto. Só este componente (montado com o menu aberto) assina as
 * fases e os estados de todos os projetos; o botão da barra não re-renderiza com eventos alheios.
 */
function SpaceMenuItems({
  spaceId,
  projects,
  attention,
  onPick,
}: {
  spaceId: string | null;
  projects: readonly ProjectEntry[];
  attention: Attention | null;
  onPick: (projectId: string) => void;
}) {
  const { t } = useT();
  const phases = useProjectPhases(projects);
  const waiting = (id: string): number => {
    const entry = attention?.projects.find((p) => p.project_id === id);
    return entry === undefined ? 0 : projectAttentionCount(entry);
  };
  return (
    <>
          {projects.length > 0 ? <div className="nd-menu-label">{t("common.projects")}</div> : null}
          {projects.map((p) => {
            const phase = phases[p.id] ?? "idle";
            const count = waiting(p.id);
            const stateText = count > 0 ? t("space.needsYou", { count }) : PHASE_LABELS[phase];
            return (
              <button
                key={p.id}
                type="button"
                role="menuitemradio"
                aria-checked={p.id === spaceId}
                className="nd-menu-item"
                data-space-item={p.id}
                onClick={() => onPick(p.id)}
              >
                <span className="nd-dot" data-tone={count > 0 ? "amber" : PHASE_TONE[phase]} aria-hidden="true" />
                <span className="nd-menu-text">{p.name}</span>
                <span className="nd-menu-hint">{stateText}</span>
                {p.id === spaceId ? <Icon name="check" size={14} /> : null}
              </button>
            );
          })}
          {projects.length > 0 ? <div className="nd-menu-sep" role="separator" /> : null}
    </>
  );
}
