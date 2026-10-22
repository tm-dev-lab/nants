/*
 * Novo projeto (spec 093, decisão 16). Pede só o nome: o servidor cria
 * `<pasta de projetos>/<slug>` com Git e registra o projeto. O nome exibido é o que o usuário
 * digitou; o slug só nomeia a pasta.
 * - Pasta já existente (409 `project_dir_exists`): oferece "Usar a pasta existente" com aquele caminho.
 * - "Usar uma pasta existente" (avançado): o navegador de pastas (`GET /api/fs/browse`).
 * - Erros fixos: nome inválido, Git ausente, pasta de projetos inválida.
 */
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { browse, type ApiFailure } from "../api/client";
import type { BrowseView, Project, ProjectEntry } from "../api/types";
import { t as translateNow } from "../i18n";
import { ptBR } from "../i18n/ptBR";
import { useT } from "../i18n/react";
import { apiFailureText } from "../i18n/serverCodes";
import { validateName } from "../settings/rules";
import { appStore } from "../store";
import { isPrereqFailure, prereqFailureMessage } from "../apps/shared/readiness";
import { createProjectAtPath, createProjectByName, type CreateProjectOutcome } from "./api";
import { Dialog } from "./Dialog";
import { useShell } from "./shellContext";
import { Icon } from "./Icon";

interface NewProjectDialogProps {
  onClose: () => void;
  /** Projeto criado (o shell troca para o espaço dele). */
  onCreated: (project: Project) => void;
}

/** Listagem fora do formato: o texto de reserva "resposta inesperada do servidor", no idioma ativo. */
const INVALID_LISTING: ApiFailure = { kind: "invalid_response", status: 200, message: ptBR["api.invalidResponse"], messageKey: "api.invalidResponse" };

/** Acrescenta o projeto à config do store sem duplicar (a criação é idempotente no servidor). */
function upsertProject(projects: readonly ProjectEntry[], project: Project): ProjectEntry[] {
  const view: ProjectEntry = { id: project.id, name: project.name, path: project.path, created_at: project.created_at };
  return projects.some((p) => p.id === view.id) ? projects.map((p) => (p.id === view.id ? view : p)) : [...projects, view];
}

/** Segmentos do caminho para a trilha: `/` e cada pasta com o caminho acumulado. */
function breadcrumbs(path: string): { label: string; path: string }[] {
  const parts = path.split("/").filter((part) => part !== "");
  const crumbs = [{ label: "/", path: "/" }];
  let acc = "";
  for (const part of parts) {
    acc = `${acc}/${part}`;
    crumbs.push({ label: part, path: acc });
  }
  return crumbs;
}

/** Projeto criado: entra no store (config) e deixa de contar como removido. */
function adoptCreatedProject(project: Project): void {
  appStore.readmitProject(project.id);
  appStore.setState((s) => ({
    config: s.config === null ? null : { ...s.config, projects: upsertProject(s.config.projects, project) },
  }));
}

/** Navegador de pastas ("Usar pasta existente"). */
function FolderBrowser({ onPick, onSeen }: { onPick: (path: string) => void; onSeen: (entries: Record<string, boolean>) => void }) {
  const hiddenId = useId();
  const [listing, setListing] = useState<BrowseView | null>(null);
  const { t } = useT();
  const [listFailure, setListFailure] = useState<ApiFailure | null>(null);
  const listError = listFailure === null ? null : apiFailureText(listFailure);
  const [hidden, setHidden] = useState(false);
  const [loading, setLoading] = useState(false);
  const mounted = useRef(true);
  const seqRef = useRef(0);

  async function list(opts: { path?: string; hidden: boolean }): Promise<void> {
    const seq = ++seqRef.current;
    setLoading(true);
    const result = await browse(opts);
    if (!mounted.current || seq !== seqRef.current) return;
    setLoading(false);
    if (!result.ok) {
      setListFailure(result.error);
      return;
    }
    if (!Array.isArray(result.data.entries) || typeof result.data.path !== "string") {
      setListFailure(INVALID_LISTING);
      return;
    }
    setListFailure(null);
    setListing(result.data);
    const seen: Record<string, boolean> = {};
    for (const entry of result.data.entries) seen[entry.path] = entry.is_git_repo;
    onSeen(seen);
  }

  useEffect(() => {
    mounted.current = true;
    void list({ hidden: false });
    return () => {
      mounted.current = false;
    };
  }, []);

  return (
    <section aria-label={t("newProject.chooseFolder")} className="nd-folder-browser" data-folder-browser="">
      {listing !== null ? (
        <nav aria-label={t("newProject.currentPath")} className="nd-crumbs">
          {breadcrumbs(listing.path).map((crumb, index) => (
            <span key={crumb.path} className="nd-crumb">
              {index > 0 ? <Icon name="chevron-right" size={14} /> : null}
              <button type="button" className="nd-link nd-mono" onClick={() => void list({ path: crumb.path, hidden })}>
                {crumb.label}
              </button>
            </span>
          ))}
        </nav>
      ) : null}
      <div className="nd-row">
        <button
          type="button"
          className="nd-btn"
          disabled={listing === null || listing.parent === null}
          onClick={() => {
            if (listing?.parent) void list({ path: listing.parent, hidden });
          }}
        >
          <Icon name="folder-up" size={14} />
          {t("newProject.parent")}
        </button>
        <label htmlFor={hiddenId} className="nd-check">
          <input
            id={hiddenId}
            type="checkbox"
            checked={hidden}
            onChange={(event) => {
              const next = event.target.checked;
              setHidden(next);
              void list({ path: listing?.path, hidden: next });
            }}
          />
          {t("newProject.showHidden")}
        </label>
      </div>
      {listError !== null ? (
        <p role="alert" className="nd-dialog-error">
          {listError}
        </p>
      ) : null}
      {loading ? (
        <p role="status" className="nd-muted">
          {t("newProject.loadingFolders")}
        </p>
      ) : null}
      {listing !== null ? (
        <>
          <ul aria-label={t("newProject.folders")} className="nd-folder-list">
            {listing.entries.map((entry) => (
              <li key={entry.path}>
                <button
                  type="button"
                  className="nd-folder-item"
                  aria-label={t("newProject.openFolder", { name: entry.name })}
                  onClick={() => void list({ path: entry.path, hidden })}
                >
                  <Icon name="folder" size={14} />
                  <span className="nd-mono">{entry.name}</span>
                  {entry.is_git_repo ? <span className="nd-tag">{t("newProject.gitRepo")}</span> : null}
                </button>
              </li>
            ))}
          </ul>
          {listing.entries.length === 0 ? <p className="nd-muted">{t("newProject.noSubfolders")}</p> : null}
          {listing.truncated ? <p className="nd-muted">{t("newProject.truncated")}</p> : null}
          <button type="button" className="nd-btn" onClick={() => onPick(listing.path)}>
            <Icon name="check" size={14} />
            {t("newProject.useThisFolder")}
          </button>
        </>
      ) : null}
    </section>
  );
}

export function NewProjectDialog({ onClose, onCreated }: NewProjectDialogProps) {
  const shell = useShell();
  const titleId = useId();
  const nameId = useId();
  const pathId = useId();
  const nameRef = useRef<HTMLInputElement | null>(null);
  const [name, setName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const { t } = useT();
  /** Erro do envio, montado na hora de exibir (no idioma ativo). */
  const [errorText, setErrorText] = useState<(() => string) | null>(null);
  const error = errorText === null ? null : errorText();
  const setError = (text: (() => string) | null): void => setErrorText(() => text);
  /** Caminho devolvido pelo 409 `project_dir_exists`. */
  const [existing, setExisting] = useState<string | null>(null);
  /** Pastas já vistas na navegação (caminho → é repositório), para a nota de "sem Git". */
  const [gitByPath, setGitByPath] = useState<Record<string, boolean>>({});
  const mounted = useRef(true);
  useEffect(
    () => () => {
      mounted.current = false;
    },
    [],
  );

  async function finish(outcome: CreateProjectOutcome): Promise<void> {
    if (!mounted.current) return;
    setBusy(false);
    if (outcome.kind === "created") {
      adoptCreatedProject(outcome.project);
      onCreated(outcome.project);
      onClose();
      return;
    }
    if (outcome.kind === "exists") {
      setExisting(outcome.path);
      return;
    }
    // 412 `prereq_missing` (fora o Git, que tem texto próprio): rótulos da prontidão relida.
    const fixed = (): string => (outcome.key !== undefined ? translateNow(outcome.key) : apiFailureText(outcome.failure));
    if (isPrereqFailure(outcome.failure) && outcome.key !== "newProject.gitMissing") {
      const prereq = await prereqFailureMessage(outcome.failure, shell.reloadReadiness, shell.readiness);
      if (!mounted.current) return;
      setError(prereq !== null ? () => prereq : fixed);
      return;
    }
    setError(fixed);
  }

  async function create(at: string | null): Promise<void> {
    if (busy) return;
    const problem = validateName(name);
    setNameError(problem);
    if (problem !== null) return;
    const trimmed = name.trim();
    if (trimmed === "") {
      setNameError(t("newProject.nameRequired"));
      return;
    }
    if (at !== null && at.trim() === "") {
      setError(() => translateNow("newProject.pathRequired"));
      return;
    }
    setBusy(true);
    setError(null);
    setExisting(null);
    await finish(at === null ? await createProjectByName(trimmed) : await createProjectAtPath(trimmed, at.trim()));
  }

  const submit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void create(advanced ? path : null);
  };

  return (
    <Dialog titleId={titleId} title={t("common.newProject")} onClose={onClose} width={520} initialFocus={nameRef}>
      <form className="nd-form" onSubmit={submit} noValidate data-new-project-form="">
        <div className="nd-field">
          <label htmlFor={nameId} className="nd-field-label">
            {t("newProject.name")}
          </label>
          <input
            id={nameId}
            ref={nameRef}
            type="text"
            className="nd-input"
            value={name}
            autoComplete="off"
            placeholder={t("newProject.namePlaceholder")}
            aria-invalid={nameError !== null}
            onChange={(event) => {
              setName(event.target.value);
              setNameError(null);
              setExisting(null);
            }}
          />
          {nameError !== null ? <p className="nd-field-error">{nameError}</p> : null}
          {!advanced ? <p className="nd-field-hint">{t("newProject.nameHint")}</p> : null}
        </div>

        {existing !== null ? (
          <div role="alert" className="nd-callout" data-dir-exists="">
            <p>
              {t("newProject.exists")} <span className="nd-mono">{existing}</span>
            </p>
            <button type="button" className="nd-btn" disabled={busy} onClick={() => void create(existing)}>
              <Icon name="folder" size={14} />
              {t("newProject.useExisting")}
            </button>
          </div>
        ) : null}

        <button
          type="button"
          className="nd-link nd-disclosure"
          aria-expanded={advanced}
          data-advanced-toggle=""
          onClick={() => setAdvanced((v) => !v)}
        >
          <Icon name={advanced ? "chevron-down" : "chevron-right"} size={14} />
          {t("newProject.advanced")}
        </button>
        {advanced ? (
          <div className="nd-advanced">
            <FolderBrowser onPick={setPath} onSeen={(seen) => setGitByPath((prev) => ({ ...prev, ...seen }))} />
            <div className="nd-field">
              <label htmlFor={pathId} className="nd-field-label">
                {t("newProject.path")}
              </label>
              <input
                id={pathId}
                type="text"
                className="nd-input nd-mono"
                value={path}
                autoComplete="off"
                spellCheck={false}
                placeholder={t("newProject.pathPlaceholder")}
                onChange={(event) => setPath(event.target.value)}
              />
            </div>
            {gitByPath[path.trim()] === false ? (
              <p className="nd-field-hint" data-no-git-note="">
                {t("newProject.noGit")}
              </p>
            ) : null}
          </div>
        ) : null}

        {error !== null ? (
          <p role="alert" className="nd-dialog-error">
            {error}
          </p>
        ) : null}
        <div className="nd-dialog-actions">
          <button type="button" className="nd-btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="nd-btn" data-variant="primary" disabled={busy} aria-busy={busy}>
            <Icon name="plus" size={14} />
            {busy ? t("newProject.creating") : t("newProject.create")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
