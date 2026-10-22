/*
 * Janela "Primeiros passos" (spec 093, decisão 15): setup guiado pelo `/api/readiness`.
 * 1. Sua máquina: o que falta, os comandos para copiar (o Nants nunca executa), "Verificar de novo"
 *    e o diagnóstico completo, o mesmo do `nants doctor` (spec 103 T02, `GET /api/system/prereqs`);
 * 2. Conexão: adicionar e testar;
 * 3. Agentes: com "Usar o mesmo modelo para todos";
 * 4. Primeiro projeto: abre o diálogo Novo projeto.
 * Os formulários são os de `apps/settings/` (reutilizados pela janela Configurações).
 */
import { useCallback, useEffect, useId, useRef, useState, type ReactElement } from "react";
import { getPrereqs } from "../../api/client";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { useAppStore } from "../../store";
import { prereqDetail, prereqFix, prereqLabel, readinessItemFix, readinessItemLabel } from "../shared/readiness";
import { AgentsForm } from "../settings/AgentsForm";
import { ConnectionRow } from "../settings/ConnectionRow";
import { ConnectionForm } from "../settings/ConnectionForm";
import {
  DIAGNOSIS_STATUS_LABELS,
  diagnosisStatus,
  firstPendingStep,
  SETUP_STEP_LABELS,
  SETUP_STEPS,
  stepDone,
  stepMissing,
  type DiagnosisItem,
  type SetupStep,
} from "./setup";

/** Copia o comando de correção (sem rede; falha silenciosa vira aviso). */
function CopyButton({ text, label }: { text: string; label: string }) {
  const { t } = useT();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <button
      type="button"
      className="nd-btn nd-btn-sm"
      aria-label={label}
      onClick={() => {
        const clip = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
        if (clip === undefined) {
          setState("failed");
          return;
        }
        clip.writeText(text).then(
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

type DiagnosisLoad = { status: "loading" } | { status: "error" } | { status: "ok"; items: DiagnosisItem[] };

/** Diagnóstico completo da máquina, o mesmo do `nants doctor` (spec 103 T02): uma linha por item. */
function MachineDiagnosis({ load, fixShownAbove }: { load: DiagnosisLoad; fixShownAbove: ReadonlySet<string> }) {
  const { t } = useT();
  if (load.status === "loading") {
    return (
      <p className="nd-muted" aria-live="polite">
        {t("setup.machine.reading")}
      </p>
    );
  }
  if (load.status === "error") {
    return <p className="nd-muted">{t("setup.machine.readFailed")}</p>;
  }
  return (
    <ul className="nd-prereq-list" aria-label={t("setup.machine.diagnosis")}>
      {load.items.map((item) => {
        const status = diagnosisStatus(item);
        const detail = item.version !== undefined && item.version !== "" ? item.version : prereqDetail(item);
        const label = prereqLabel(item);
        const fix = item.ok || fixShownAbove.has(item.id) ? undefined : prereqFix(item);
        return (
          <li key={item.id} className="nd-prereq" data-prereq={item.id} data-status={status} data-ok={item.ok ? "true" : "false"}>
            <span className="nd-row">
              <span className="nd-prereq-mark" data-ok={item.ok ? "true" : "false"}>
                <Icon name={status === "ok" ? "check" : status === "info" ? "info" : "alert"} size={14} />
              </span>
              <span className="nd-strong">{label}</span>
              <span className="nd-sr-only">{`: ${DIAGNOSIS_STATUS_LABELS[status]}`}</span>
            </span>
            {detail !== undefined && detail !== "" ? <p className="nd-muted">{detail}</p> : null}
            {fix !== undefined ? (
              <span className="nd-row">
                <code className="nd-mono nd-code">{fix}</code>
                <CopyButton text={fix} label={t("common.copyCommandFor", { label })} />
              </span>
            ) : null}
          </li>
        );
      })}
    </ul>
  );
}

function MachineStep() {
  const { t } = useT();
  const shell = useShell();
  const readiness = shell.readiness;
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [diagnosis, setDiagnosis] = useState<DiagnosisLoad>({ status: "loading" });
  const seq = useRef(0);
  const alive = useRef(true);
  const reloadDiagnosis = useCallback(async (): Promise<void> => {
    const mine = ++seq.current;
    const result = await getPrereqs();
    if (!alive.current || mine !== seq.current) return;
    setDiagnosis(result.ok ? { status: "ok", items: result.data } : { status: "error" });
  }, []);
  useEffect(() => {
    alive.current = true;
    void reloadDiagnosis();
    return () => {
      alive.current = false;
    };
  }, [reloadDiagnosis]);
  const missing = readiness === null ? [] : stepMissing(readiness, "machine");
  // O comando de correção das pendências já aparece na lista acima; o diagnóstico não o repete.
  const missingIds: ReadonlySet<string> = new Set(missing.map((m) => m.id));
  return (
    <div className="nd-setup-step" data-setup-step="machine">
      <p className="nd-field-hint">{t("settings.machine.intro")}</p>
      {missing.length === 0 ? (
        <p className="nd-ok" role="status">
          <Icon name="check" size={14} />
          {t("setup.machine.ready")}
        </p>
      ) : (
        <ul className="nd-missing-list" aria-label={t("setup.machine.missing")}>
          {missing.map((item) => {
            const label = readinessItemLabel(item);
            const fix = readinessItemFix(item);
            return (
              <li key={item.id} className="nd-missing" data-missing={item.id}>
                <span className="nd-row">
                  <Icon name="alert" size={14} />
                  <span className="nd-strong">{label}</span>
                </span>
                {fix !== undefined ? (
                  <span className="nd-row">
                    <code className="nd-mono nd-code">{fix}</code>
                    <CopyButton text={fix} label={t("common.copyCommandFor", { label })} />
                  </span>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      <h3 className="nd-strong">{t("setup.machine.fullDiagnosis")}</h3>
      <p className="nd-field-hint">{t("setup.machine.doctorHint")}</p>
      <MachineDiagnosis load={diagnosis} fixShownAbove={missingIds} />
      {failed ? (
        <p role="alert" className="nd-dialog-error">
          {t("setup.machine.recheckFailed")}
        </p>
      ) : null}
      <div className="nd-row">
        <button
          type="button"
          className="nd-btn"
          disabled={busy}
          aria-busy={busy}
          data-recheck=""
          onClick={() => {
            setBusy(true);
            setFailed(false);
            void shell
              .recheckMachine()
              .then(async (ok) => {
                await reloadDiagnosis();
                return ok;
              })
              .then((ok) => {
                if (!alive.current) return;
                setBusy(false);
                setFailed(!ok);
              });
          }}
        >
          <Icon name="refresh" size={14} />
          {busy ? t("common.checking") : t("common.checkAgain")}
        </button>
      </div>
    </div>
  );
}

function ConnectionStep() {
  const { t } = useT();
  const shell = useShell();
  const connections = useAppStore((s) => s.config?.connections) ?? [];
  const [adding, setAdding] = useState(false);
  const showForm = adding || connections.length === 0;
  return (
    <div className="nd-setup-step" data-setup-step="connection">
      <p className="nd-field-hint">{t("setup.connection.hint")}</p>
      {connections.length > 0 ? (
        <ul className="nd-conn-list" aria-label={t("settings.savedConnections")}>
          {connections.map((c) => (
            <ConnectionRow key={c.id} connection={c} />
          ))}
        </ul>
      ) : null}
      {showForm ? (
        <ConnectionForm
          existingIds={connections.map((c) => c.id)}
          onSaved={() => {
            setAdding(false);
            void shell.reloadReadiness();
          }}
          {...(connections.length > 0 ? { onCancel: () => setAdding(false) } : {})}
        />
      ) : (
        <button type="button" className="nd-btn" onClick={() => setAdding(true)}>
          <Icon name="plus" size={14} />
          {t("setup.connection.add")}
        </button>
      )}
    </div>
  );
}

function AgentsStep() {
  const { t } = useT();
  const shell = useShell();
  const config = useAppStore((s) => s.config);
  if (config === null) {
    return (
      <p role="status" className="nd-field-hint">
        {t("windowEmpty.settings")}
      </p>
    );
  }
  return (
    <div className="nd-setup-step" data-setup-step="agents">
      <p className="nd-field-hint">{t("setup.agents.hint")}</p>
      {config.connections.length === 0 ? (
        <p className="nd-field-warn">{t("setup.agents.needConnection")}</p>
      ) : (
        <AgentsForm agents={config.agents} connections={config.connections} onSaved={() => void shell.reloadReadiness()} />
      )}
    </div>
  );
}

function ProjectStep() {
  const { t, tp } = useT();
  const shell = useShell();
  const count = useAppStore((s) => s.config?.projects.length ?? 0);
  const missing = shell.readiness?.create_project.missing ?? [];
  return (
    <div className="nd-setup-step" data-setup-step="project">
      <p className="nd-field-hint">{t("setup.project.hint")}</p>
      {count > 0 ? (
        <p className="nd-ok" role="status">
          <Icon name="check" size={14} />
          {tp("setup.project.count", count)}
        </p>
      ) : null}
      {missing.length > 0 ? (
        <p className="nd-field-warn">{t("setup.project.beforeCreating", { items: missing.map(readinessItemLabel).join(", ") })}</p>
      ) : null}
      <button type="button" className="nd-btn" data-variant="primary" onClick={shell.openNewProject}>
        <Icon name="plus" size={14} />
        {count > 0 ? t("common.newProject") : t("setup.project.createFirst")}
      </button>
    </div>
  );
}

const STEP_VIEWS: Readonly<Record<SetupStep, () => ReactElement>> = {
  machine: MachineStep,
  connection: ConnectionStep,
  agents: AgentsStep,
  project: ProjectStep,
};

export function SetupApp() {
  const { t } = useT();
  const shell = useShell();
  const readiness = shell.readiness;
  const projectCount = useAppStore((s) => s.config?.projects.length ?? 0);
  const headingId = useId();
  const [step, setStep] = useState<SetupStep | null>(null);
  // Sem escolha do usuário, a etapa aberta é a primeira pendente.
  const current = step ?? firstPendingStep(readiness, projectCount);
  useEffect(() => {
    if (readiness === null) void shell.reloadReadiness();
    // Só na montagem: o shell já relê a prontidão nos demais momentos.
  }, []);
  // A etapa aberta fica fixa depois da primeira leitura (concluir uma etapa não pula a tela).
  useEffect(() => {
    if (step === null && readiness !== null) setStep(firstPendingStep(readiness, projectCount));
  }, [step, readiness, projectCount]);

  if (readiness === null) {
    return (
      <p role="status" className="nd-setup-loading">
        {t("windowEmpty.gettingStarted")}
      </p>
    );
  }
  const index = SETUP_STEPS.indexOf(current);
  const View = STEP_VIEWS[current];
  return (
    <div className="nd-setup" data-setup="">
      <nav aria-label={t("setup.steps")} className="nd-steps">
        <ol>
          {SETUP_STEPS.map((s, i) => {
            const done = stepDone(readiness, s, projectCount);
            return (
              <li key={s}>
                <button
                  type="button"
                  className="nd-step"
                  aria-current={s === current ? "step" : undefined}
                  data-done={done ? "true" : undefined}
                  data-step={s}
                  onClick={() => setStep(s)}
                >
                  <span className="nd-step-mark" aria-hidden="true">
                    {done ? <Icon name="check" size={14} /> : i + 1}
                  </span>
                  <span>{SETUP_STEP_LABELS[s]}</span>
                  <span className="nd-sr-only">{done ? t("setup.stepDone") : t("setup.stepPending")}</span>
                </button>
              </li>
            );
          })}
        </ol>
      </nav>
      <section aria-labelledby={headingId} className="nd-setup-body">
        <h2 id={headingId} className="nd-setup-title">
          {index + 1}. {SETUP_STEP_LABELS[current]}
        </h2>
        <View />
      </section>
      <div className="nd-dialog-actions">
        {index > 0 ? (
          <button type="button" className="nd-btn" onClick={() => setStep(SETUP_STEPS[index - 1] ?? current)}>
            {t("setup.previous")}
          </button>
        ) : null}
        {index < SETUP_STEPS.length - 1 ? (
          <button type="button" className="nd-btn" data-variant="primary" onClick={() => setStep(SETUP_STEPS[index + 1] ?? current)}>
            {t("setup.next")}
            <Icon name="arrow-right" size={14} />
          </button>
        ) : null}
      </div>
    </div>
  );
}
