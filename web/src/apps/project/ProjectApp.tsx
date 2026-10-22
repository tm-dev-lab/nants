/*
 * Janela Projeto (`DEFAULT_APPS.project.Content`, em `memo`): os ajustes do projeto ativo.
 * - Pasta do projeto (com "Copiar");
 * - Limites: tentativas do Desenvolvedor por tarefa nova e ciclos de teste por tentativa (1 a 10).
 *   Sem edição pendente, o formulário reflete limites mudados de fora (outra aba); a edição
 *   pendente de um campo é preservada;
 * - Modo YOLO e Internet do Arquiteto, com as mesmas confirmações da barra superior;
 * - "Remover do Nants" com a confirmação de hoje (inclusive os volumes Docker). A pasta nunca é
 *   apagada. Removido (aqui ou em outra aba), o shell fecha o espaço, mostra o aviso e vai para
 *   outro espaço ou para as boas-vindas.
 * Responsiva por `data-size`: estreita, os campos dos limites ficam empilhados.
 */
import "./project.css";
import { memo, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { putProjectLimits } from "../../api/client";
import type { ProjectState } from "../../api/types";
import { t as translateNow } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText, messageText } from "../../i18n/serverCodes";
import { EmptyState } from "../../desktop/EmptyState";
import { Icon } from "../../desktop/Icon";
import { useSpace } from "../../desktop/shellContext";
import { InternetToggle, YoloToggle } from "../../desktop/topbar/ProjectToggles";
import { appStore, useAppStore } from "../../store";
import type { AppContentProps } from "../registry";
import "../shared/apps.css";
import { LIMIT_MAX, LIMIT_MIN, parseLimit } from "./projectModel";
import { RemoveProjectDialog } from "./RemoveProjectDialog";

function CopyPath({ path }: { path: string }) {
  const { t } = useT();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <button
      type="button"
      className="nd-btn nd-btn-sm"
      aria-label={t("project.copyPath")}
      onClick={() => {
        const clip = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
        if (clip === undefined) {
          setState("failed");
          return;
        }
        clip.writeText(path).then(
          () => setState("copied"),
          () => setState("failed"),
        );
      }}
    >
      <Icon name={state === "copied" ? "check" : "copy"} size={14} />
      {state === "copied" ? t("common.copied") : state === "failed" ? t("common.copyManually") : t("common.copy")}
    </button>
  );
}

function LimitsForm({ projectId, state }: { projectId: string; state: ProjectState }) {
  const attemptsId = useId();
  const cyclesId = useId();
  const hintId = useId();
  const [attempts, setAttempts] = useState(String(state.max_attempts));
  const [cycles, setCycles] = useState(String(state.max_test_cycles));
  // Limites vindos de fora (outra aba, outro formulário): cada campo sem edição pendente (igual ao
  // último valor conhecido do servidor) acompanha o novo valor; um campo editado é preservado.
  const [known, setKnown] = useState({ attempts: state.max_attempts, cycles: state.max_test_cycles });
  if (known.attempts !== state.max_attempts || known.cycles !== state.max_test_cycles) {
    if (attempts === String(known.attempts)) setAttempts(String(state.max_attempts));
    if (cycles === String(known.cycles)) setCycles(String(state.max_test_cycles));
    setKnown({ attempts: state.max_attempts, cycles: state.max_test_cycles });
  }
  const [busy, setBusy] = useState(false);
  const { t } = useT();
  // Mensagem montada na hora de exibir (no idioma ativo).
  const [message, setMessage] = useState<{ ok: boolean; text: () => string } | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  async function save(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const a = parseLimit(attempts);
    const c = parseLimit(cycles);
    if (a === null || c === null) {
      setMessage({ ok: false, text: () => translateNow("project.limitsInvalid") });
      return;
    }
    setBusy(true);
    setMessage(null);
    const result = await putProjectLimits(projectId, { max_attempts: a, max_test_cycles: c });
    if (!alive.current) return;
    setBusy(false);
    if (!result.ok) {
      const failure = result.error;
      setMessage({ ok: false, text: () => (failure.status === 409 ? translateNow("project.limitsBusy") : apiFailureText(failure)) });
      return;
    }
    const data = result.data;
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [projectId]: data } }));
    setMessage({ ok: true, text: () => translateNow("project.limitsSaved") });
  }

  return (
    <form className="nd-form" aria-label={t("project.limitsForm")} onSubmit={(e) => void save(e)} noValidate>
      <div className="nd-project-limits">
        <div className="nd-field">
          <label htmlFor={attemptsId} className="nd-field-label">
            {t("project.attempts")}
          </label>
          <input id={attemptsId} className="nd-input" inputMode="numeric" value={attempts} aria-describedby={hintId} onChange={(e) => setAttempts(e.target.value)} />
        </div>
        <div className="nd-field">
          <label htmlFor={cyclesId} className="nd-field-label">
            {t("project.cycles")}
          </label>
          <input id={cyclesId} className="nd-input" inputMode="numeric" value={cycles} aria-describedby={hintId} onChange={(e) => setCycles(e.target.value)} />
        </div>
      </div>
      <p id={hintId} className="nd-field-hint">
        {t("project.limitsHint", { min: LIMIT_MIN, max: LIMIT_MAX })}
      </p>
      {message !== null ? (
        <p role={message.ok ? "status" : "alert"} className={message.ok ? "nd-ok" : "nd-field-error"}>
          {message.ok ? <Icon name="check" size={14} /> : null}
          {message.text()}
        </p>
      ) : null}
      <div className="nd-dialog-actions">
        <button type="submit" className="nd-btn" data-variant="primary" disabled={busy} aria-busy={busy}>
          <Icon name="save" size={14} />
          {t("project.saveLimits")}
        </button>
      </div>
    </form>
  );
}

function ProjectContentView({ size }: AppContentProps) {
  const { t } = useT();
  const space = useSpace();
  const projectId = space?.data.projectId ?? "";
  const entry = useAppStore((s) => s.config?.projects.find((p) => p.id === projectId) ?? null);
  const [removing, setRemoving] = useState(false);
  const folderId = useId();
  const limitsId = useId();
  const modesId = useId();
  const removeId = useId();
  if (space === null) return null;
  const { data } = space;
  const state = data.state;
  if (state === null) {
    return <EmptyState icon="project" message={data.loadError !== null ? messageText(data.loadError) : t("project.loading")} />;
  }
  const path = entry?.path ?? data.project?.path ?? null;
  return (
    <div className="nd-app-scroll nd-app-page nd-project" data-project="" data-size={size}>
      <section aria-labelledby={folderId} className="nd-project-section">
        <h3 id={folderId} className="nd-section-label">
          {t("project.folder")}
        </h3>
        {path !== null ? (
          <div className="nd-row">
            <code className="nd-code nd-mono nd-project-path" data-project-path="">
              {path}
            </code>
            <CopyPath path={path} />
          </div>
        ) : (
          <p className="nd-muted">{t("project.pathUnavailable")}</p>
        )}
        <p className="nd-field-hint">{t("project.folderHint")}</p>
      </section>

      <section aria-labelledby={limitsId} className="nd-project-section">
        <h3 id={limitsId} className="nd-section-label">
          {t("project.limits")}
        </h3>
        <LimitsForm key={projectId} projectId={projectId} state={state} />
      </section>

      <section aria-labelledby={modesId} className="nd-project-section">
        <h3 id={modesId} className="nd-section-label">
          {t("project.modes")}
        </h3>
        <div className="nd-project-mode">
          <YoloToggle projectId={projectId} state={state} onChanged={data.reload} />
          <p className="nd-field-hint">{t("project.yoloHint")}</p>
        </div>
        <div className="nd-project-mode">
          <InternetToggle projectId={projectId} state={state} />
          <p className="nd-field-hint">{t("internet.explanation")}</p>
        </div>
      </section>

      <section aria-labelledby={removeId} className="nd-project-section">
        <h3 id={removeId} className="nd-section-label" data-tone="rose">
          {t("project.remove")}
        </h3>
        <p className="nd-field-hint">{t("project.removeHint")}</p>
        <button type="button" className="nd-btn nd-self-start" data-variant="danger" onClick={() => setRemoving(true)}>
          <Icon name="trash" size={14} />
          {t("project.remove")}
        </button>
      </section>
      {removing ? (
        <RemoveProjectDialog project={{ id: projectId, name: data.name, path: path ?? "" }} onClose={() => setRemoving(false)} />
      ) : null}
    </div>
  );
}

export const ProjectContent = memo(ProjectContentView);
