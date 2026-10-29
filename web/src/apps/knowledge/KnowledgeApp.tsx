/*
 * Janela Conhecimento (spec 134 T06, `DEFAULT_APPS.knowledge.Content`):
 * - abas "Documentos" e "Visão geral" e o "Atualizar" no slot da barra de título;
 * - vazia: a área de soltar com "Adicionar arquivos", o campo de link e o aviso de que a base
 *   fica fora do Git e é dado, não instrução;
 * - com documentos: a lista (ícone do tipo, título, origem, tamanho, estado, marcação e
 *   indicadores) e a prévia da nota em markdown, lado a lado; estreita (celular), uma de cada vez,
 *   com "Voltar";
 * - na prévia: a marcação Padrão/Referência, a descrição e o `applies_to` (só nos Padrões)
 *   editáveis, a remoção com confirmação e a seção citada pelo plano em destaque;
 * - a lista é compartilhada com o subtítulo vivo (`kbStore`) e se atualiza por `kb_updated`.
 */
import "./knowledge.css";
import "../shared/apps.css";
import { memo, useEffect, useId, useMemo, useRef, useState, type DragEvent as ReactDragEvent, type FormEvent, type ReactNode } from "react";
import type { ApiFailure } from "../../api/client";
import {
  deleteKbDoc,
  getKbDoc,
  getKbOverview,
  patchKbDoc,
  postKbFile,
  postKbLink,
  putKbOverview,
  type KbDoc,
  type KbNote,
  type KbPatch,
  type KbRole,
} from "../../api/kb";
import { EmptyState } from "../../desktop/EmptyState";
import { Icon } from "../../desktop/Icon";
import { useSpace } from "../../desktop/shellContext";
import type { ContainerSize } from "../../desktop/useContainerSize";
import { WindowTools } from "../../desktop/Window";
import { formatDate, formatNumber } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { MarkdownText } from "../shared/ReportMarkdown";
import type { AppContentProps } from "../registry";
import { reloadKb, removeKbDoc, setKbOverviewFlag, upsertKbDoc, useKb } from "./kbStore";
import {
  detailParts,
  indicatorTexts,
  kbArrangement,
  KB_MAX_DESCRIPTION_CHARS,
  KB_MAX_UPLOAD_MIB,
  kindIcon,
  reasonText,
  splitAtSection,
  STATUS_TONE,
  statusText,
} from "./knowledgeModel";
import { consumeOpenKbDoc, useOpenKbDocRequest } from "./openRequest";

type Tab = "docs" | "overview";

interface Upload {
  key: number;
  name: string;
  failure: ApiFailure | null;
}

let uploadSeq = 0;

// --- Envio: área de soltar, botão e link ---

function AddArea({ projectId, empty }: { projectId: string; empty: boolean }) {
  const { t } = useT();
  const [over, setOver] = useState(false);
  const [uploads, setUploads] = useState<Upload[]>([]);
  const [url, setUrl] = useState("");
  const [linkBusy, setLinkBusy] = useState(false);
  const [linkFailure, setLinkFailure] = useState<ApiFailure | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  const linkId = useId();
  const linkHintId = useId();

  async function send(files: readonly File[]): Promise<void> {
    for (const file of files) {
      uploadSeq += 1;
      const key = uploadSeq;
      setUploads((list) => [...list, { key, name: file.name, failure: null }]);
      const result = await postKbFile(projectId, file, file.name);
      if (result.ok) {
        upsertKbDoc(projectId, result.data.doc);
        setUploads((list) => list.filter((u) => u.key !== key));
      } else {
        setUploads((list) => list.map((u) => (u.key === key ? { ...u, failure: result.error } : u)));
      }
    }
    void reloadKb(projectId);
  }

  function onDrop(event: ReactDragEvent<HTMLDivElement>): void {
    event.preventDefault();
    event.stopPropagation();
    setOver(false);
    void send(Array.from(event.dataTransfer.files));
  }

  function onDragOver(event: ReactDragEvent<HTMLDivElement>): void {
    if (!Array.from(event.dataTransfer.types).includes("Files")) return;
    event.preventDefault();
    event.stopPropagation();
    setOver(true);
  }

  async function addLink(event: FormEvent): Promise<void> {
    event.preventDefault();
    const value = url.trim();
    if (value === "" || linkBusy) return;
    setLinkBusy(true);
    setLinkFailure(null);
    const result = await postKbLink(projectId, value);
    setLinkBusy(false);
    if (result.ok) {
      setUrl("");
      upsertKbDoc(projectId, result.data.doc);
    } else {
      setLinkFailure(result.error);
    }
  }

  return (
    <div className="nd-kb-add" data-kb-add="" data-empty={empty ? "true" : undefined}>
      <div
        className="nd-kb-drop"
        data-kb-drop=""
        data-over={over ? "true" : undefined}
        onDragOver={onDragOver}
        onDragLeave={() => setOver(false)}
        onDrop={onDrop}
      >
        <Icon name="upload" size={empty ? 20 : 16} className="nd-kb-drop-icon" />
        <div className="nd-kb-drop-text">
          <span className="nd-kb-drop-title">{t("kb.add.drop")}</span>
          <span className="nd-muted">{t("kb.add.formats", { max: KB_MAX_UPLOAD_MIB })}</span>
        </div>
        <button type="button" className="nd-btn" data-variant={empty ? "primary" : undefined} onClick={() => input.current?.click()}>
          <Icon name="plus" size={14} />
          {t("kb.add.files")}
        </button>
        <input
          ref={input}
          type="file"
          multiple
          hidden
          accept=".md,.markdown,.txt,.pdf,text/markdown,text/plain,application/pdf"
          onChange={(e) => {
            const files = Array.from(e.target.files ?? []);
            e.target.value = "";
            void send(files);
          }}
        />
      </div>
      <form className="nd-kb-link" onSubmit={(e) => void addLink(e)}>
        <label htmlFor={linkId} className="nd-sr-only">
          {t("kb.add.link")}
        </label>
        <Icon name="link" size={14} className="nd-kb-link-icon" />
        <input
          id={linkId}
          className="nd-input nd-kb-link-input"
          type="text"
          inputMode="url"
          autoComplete="off"
          spellCheck={false}
          placeholder={t("kb.add.link")}
          value={url}
          aria-describedby={linkHintId}
          onChange={(e) => setUrl(e.target.value)}
        />
        <button type="submit" className="nd-btn" disabled={url.trim() === "" || linkBusy} aria-busy={linkBusy}>
          {t("kb.add.linkButton")}
        </button>
        <span id={linkHintId} className={empty ? "nd-muted nd-kb-link-hint" : "nd-sr-only"}>
          {t("kb.add.linkHint")}
        </span>
      </form>
      {linkFailure !== null ? (
        <p role="alert" className="nd-field-error nd-kb-note">
          {apiFailureText(linkFailure)}
        </p>
      ) : null}
      {uploads.length > 0 ? (
        <ul className="nd-kb-uploads" aria-live="polite">
          {uploads.map((u) => (
            <li key={u.key} className="nd-kb-upload" data-failed={u.failure !== null ? "true" : undefined}>
              {u.failure === null ? (
                <span className="nd-muted">{t("kb.add.uploading", { name: u.name })}</span>
              ) : (
                <>
                  <span role="alert" className="nd-field-error">
                    {t("kb.add.failed", { name: u.name, message: apiFailureText(u.failure) })}
                  </span>
                  <button
                    type="button"
                    className="nd-btn nd-iconbtn nd-btn-sm"
                    data-variant="ghost"
                    aria-label={t("common.dismiss")}
                    onClick={() => setUploads((list) => list.filter((x) => x.key !== u.key))}
                  >
                    <Icon name="close" size={14} />
                  </button>
                </>
              )}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

// --- Lista ---

function StatusPill({ doc }: { doc: KbDoc }) {
  return (
    <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone={STATUS_TONE[doc.status]} data-kb-status={doc.status}>
      {doc.status === "extracting" || doc.status === "pending" ? <span className="nd-kb-spin" aria-hidden="true" /> : null}
      {statusText(doc.status)}
    </span>
  );
}

function RolePill({ role }: { role: KbRole }) {
  const { t } = useT();
  return role === "standard" ? (
    <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="violet" data-kb-role="standard">
      {t("kb.role.standard")}
    </span>
  ) : (
    <span className="nd-kb-role-ref" data-kb-role="reference">
      {t("kb.role.reference")}
    </span>
  );
}

function DocRow({ doc, docs, selected, onOpen }: { doc: KbDoc; docs: readonly KbDoc[]; selected: boolean; onOpen: () => void }) {
  const reason = doc.status === "error" ? reasonText(doc) : null;
  const flags = indicatorTexts(doc, docs);
  return (
    <li className="nd-kb-row" data-kb-doc={doc.id} aria-current={selected ? "true" : undefined}>
      <button type="button" className="nd-kb-row-btn" onClick={onOpen}>
        <Icon name={kindIcon(doc)} size={16} className="nd-kb-kind" />
        <span className="nd-kb-row-main">
          <span className="nd-kb-row-head">
            <span className="nd-kb-title">{doc.title}</span>
            <RolePill role={doc.role} />
          </span>
          <span className="nd-kb-meta">{detailParts(doc).join(" · ")}</span>
          {reason !== null ? <span className="nd-kb-reason">{reason}</span> : null}
          {flags.length > 0 ? (
            <span className="nd-kb-flags">
              {flags.map((f) => (
                <span key={f} className="nd-kb-flag">
                  <Icon name="alert" size={14} />
                  <span>{f}</span>
                </span>
              ))}
            </span>
          ) : null}
        </span>
        <StatusPill doc={doc} />
      </button>
    </li>
  );
}

// --- Prévia ---

function NoteBody({ note, section }: { note: KbNote; section: string | null }) {
  const { t } = useT();
  const cited = useRef<HTMLDivElement | null>(null);
  const sections = note.sections;
  const parts = useMemo(() => splitAtSection(note.content, sections ?? [], section), [note.content, sections, section]);
  useEffect(() => {
    cited.current?.scrollIntoView?.({ block: "start" });
  }, [parts]);
  if (parts === null) return <MarkdownText text={note.content} />;
  return (
    <>
      {parts[0].trim() !== "" ? <MarkdownText text={parts[0]} /> : null}
      <div ref={cited} className="nd-kb-cited" data-kb-cited={section ?? undefined}>
        <span className="nd-kb-cited-label">
          <Icon name="plan" size={14} />
          {t("kb.preview.cited")}
        </span>
        <MarkdownText text={parts[1]} />
      </div>
      {parts[2].trim() !== "" ? <MarkdownText text={parts[2]} /> : null}
    </>
  );
}

function EditableLine({
  label,
  hint,
  value,
  emptyText,
  multiline,
  onSave,
}: {
  label: string;
  hint: string;
  value: string;
  emptyText: string;
  multiline: boolean;
  onSave: (value: string) => Promise<ApiFailure | null>;
}) {
  const { t } = useT();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const fieldId = useId();
  const hintId = useId();

  async function save(event: FormEvent): Promise<void> {
    event.preventDefault();
    setBusy(true);
    const result = await onSave(draft);
    setBusy(false);
    setFailure(result);
    if (result === null) setEditing(false);
  }

  if (!editing) {
    return (
      <div className="nd-kb-field" data-kb-field={label}>
        <div className="nd-kb-field-head">
          <span className="nd-section-label">{label}</span>
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            data-variant="ghost"
            aria-label={t("kb.editField", { field: label })}
            onClick={() => {
              setDraft(value);
              setFailure(null);
              setEditing(true);
            }}
          >
            <Icon name="pencil" size={14} />
            {t("kb.edit")}
          </button>
        </div>
        {multiline && value !== "" ? (
          <ul className="nd-kb-globs">
            {value.split("\n").map((g) => (
              <li key={g} className="nd-mono nd-kb-glob">
                {g}
              </li>
            ))}
          </ul>
        ) : (
          <p className={value === "" ? "nd-muted nd-kb-field-value" : "nd-kb-field-value"}>{value === "" ? emptyText : value}</p>
        )}
      </div>
    );
  }
  return (
    <form className="nd-kb-field" data-kb-field={label} data-editing="true" onSubmit={(e) => void save(e)}>
      <label htmlFor={fieldId} className="nd-section-label">
        {label}
      </label>
      {multiline ? (
        <textarea id={fieldId} className="nd-input nd-textarea nd-mono nd-kb-globs-input" rows={4} value={draft} aria-describedby={hintId} onChange={(e) => setDraft(e.target.value)} />
      ) : (
        <input id={fieldId} className="nd-input" value={draft} maxLength={KB_MAX_DESCRIPTION_CHARS} aria-describedby={hintId} onChange={(e) => setDraft(e.target.value)} />
      )}
      <span id={hintId} className="nd-field-hint">
        {hint}
      </span>
      {failure !== null ? (
        <span role="alert" className="nd-field-error">
          {apiFailureText(failure)}
        </span>
      ) : null}
      <div className="nd-row nd-wrap">
        <button type="submit" className="nd-btn nd-btn-sm" data-variant="primary" disabled={busy} aria-busy={busy}>
          {busy ? t("kb.saving") : t("kb.save")}
        </button>
        <button type="button" className="nd-btn nd-btn-sm" disabled={busy} onClick={() => setEditing(false)}>
          {t("common.cancel")}
        </button>
      </div>
    </form>
  );
}

function Preview({ projectId, doc, docs, section, onRemoved }: { projectId: string; doc: KbDoc; docs: readonly KbDoc[]; section: string | null; onRemoved: () => void }) {
  const { t } = useT();
  const [note, setNote] = useState<{ status: "loading" } | { status: "error"; failure: ApiFailure } | { status: "ok"; note: KbNote }>({ status: "loading" });
  const [confirming, setConfirming] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [problem, setProblem] = useState<ApiFailure | null>(null);
  const roleHintId = useId();
  const ready = doc.status === "ready";

  useEffect(() => {
    if (!ready) return;
    let live = true;
    setNote({ status: "loading" });
    void getKbDoc(projectId, doc.id).then((result) => {
      if (!live) return;
      setNote(result.ok ? { status: "ok", note: result.data } : { status: "error", failure: result.error });
    });
    return () => {
      live = false;
    };
  }, [projectId, doc.id, ready]);

  async function patch(p: KbPatch): Promise<ApiFailure | null> {
    const result = await patchKbDoc(projectId, doc.id, p);
    if (!result.ok) return result.error;
    upsertKbDoc(projectId, result.data.doc);
    return null;
  }

  async function setRole(role: KbRole): Promise<void> {
    if (role === doc.role) return;
    setProblem(await patch(role === "reference" ? { role, applies_to: [] } : { role }));
  }

  async function remove(): Promise<void> {
    setRemoving(true);
    const result = await deleteKbDoc(projectId, doc.id);
    setRemoving(false);
    if (!result.ok) {
      setProblem(result.error);
      setConfirming(false);
      return;
    }
    removeKbDoc(projectId, doc.id);
    onRemoved();
  }

  const reason = reasonText(doc);
  const flags = indicatorTexts(doc, docs);
  let body: ReactNode;
  if (!ready) {
    body = <p className="nd-muted nd-kb-note">{doc.status === "pending" || doc.status === "extracting" ? t("kb.preview.pending") : (reason ?? statusText(doc.status))}</p>;
  } else if (note.status === "loading") {
    body = (
      <p role="status" className="nd-muted nd-kb-note">
        {t("kb.preview.loading")}
      </p>
    );
  } else if (note.status === "error") {
    body = (
      <p role="alert" className="nd-field-error nd-kb-note">
        {t("kb.preview.error", { message: apiFailureText(note.failure) })}
      </p>
    );
  } else {
    body = <NoteBody note={note.note} section={section} />;
  }

  return (
    <article className="nd-kb-preview nd-kb-scroll" data-kb-preview={doc.id} aria-labelledby={`kb-${doc.id}-title`}>
      <header className="nd-kb-preview-head">
        <div className="nd-kb-preview-title">
          <Icon name={kindIcon(doc)} size={18} className="nd-kb-kind" />
          <h2 id={`kb-${doc.id}-title`} className="nd-kb-heading">
            {doc.title}
          </h2>
          <StatusPill doc={doc} />
          <span className="nd-kb-spacer" />
          {doc.source_url !== null ? (
            <a className="nd-btn nd-iconbtn nd-btn-sm" data-variant="ghost" href={doc.source_url} target="_blank" rel="noreferrer noopener" aria-label={t("kb.preview.openSource")} title={t("kb.preview.openSource")}>
              <Icon name="external" size={14} />
            </a>
          ) : null}
          <button
            type="button"
            className="nd-btn nd-iconbtn nd-btn-sm"
            data-variant="ghost"
            aria-label={t("kb.removeLabel", { title: doc.title })}
            title={t("kb.remove")}
            onClick={() => setConfirming(true)}
          >
            <Icon name="trash" size={14} />
          </button>
        </div>
        <p className="nd-kb-meta">
          {[...detailParts(doc), doc.fetched_at !== null ? t("kb.fetched", { date: formatDate(doc.fetched_at) }) : t("kb.added", { date: formatDate(doc.added_at) })].join(" · ")}
        </p>
        {confirming ? (
          <div role="group" className="nd-kb-confirm" data-kb-confirm="">
            <span>{t("kb.remove.confirm", { title: doc.title })}</span>
            <div className="nd-row nd-wrap">
              <button type="button" className="nd-btn nd-btn-sm" data-variant="danger" disabled={removing} aria-busy={removing} onClick={() => void remove()}>
                <Icon name="trash" size={14} />
                {t("kb.remove")}
              </button>
              <button type="button" className="nd-btn nd-btn-sm" disabled={removing} onClick={() => setConfirming(false)}>
                {t("common.cancel")}
              </button>
            </div>
          </div>
        ) : null}
        {problem !== null ? (
          <p role="alert" className="nd-field-error">
            {apiFailureText(problem)}
          </p>
        ) : null}
        {doc.status === "error" && reason !== null ? <p className="nd-kb-reason">{reason}</p> : null}
        {flags.length > 0 ? (
          <ul className="nd-kb-flags nd-kb-flags-block">
            {flags.map((f) => (
              <li key={f} className="nd-kb-flag">
                <Icon name="alert" size={14} />
                <span>{f}</span>
              </li>
            ))}
          </ul>
        ) : null}
        <div className="nd-kb-field">
          <span className="nd-section-label">{t("kb.role.label")}</span>
          <div role="group" aria-label={t("kb.role.label")} aria-describedby={roleHintId} className="nd-segment nd-chrome">
            <button type="button" className="nd-segment-item" aria-pressed={doc.role === "standard"} onClick={() => void setRole("standard")}>
              <span>{t("kb.role.standardLong")}</span>
            </button>
            <button type="button" className="nd-segment-item" aria-pressed={doc.role === "reference"} onClick={() => void setRole("reference")}>
              <span>{t("kb.role.reference")}</span>
            </button>
          </div>
          <span id={roleHintId} className="nd-field-hint">
            {doc.role === "standard" ? t("kb.role.hint.standard") : t("kb.role.hint.reference")}
          </span>
        </div>
        <EditableLine
          key={`d-${doc.id}`}
          label={t("kb.description.label")}
          hint={t("kb.description.hint", { max: KB_MAX_DESCRIPTION_CHARS })}
          value={doc.description}
          emptyText={t("kb.description.empty")}
          multiline={false}
          onSave={(value) => patch({ description: value.trim() })}
        />
        {doc.role === "standard" ? (
          <EditableLine
            key={`a-${doc.id}`}
            label={t("kb.appliesTo.label")}
            hint={t("kb.appliesTo.hint")}
            value={doc.applies_to.join("\n")}
            emptyText={t("kb.appliesTo.all")}
            multiline
            onSave={(value) =>
              patch({
                applies_to: value
                  .split("\n")
                  .map((g) => g.trim())
                  .filter((g) => g !== ""),
              })
            }
          />
        ) : null}
      </header>
      <section className="nd-kb-note-body" aria-label={t("kb.preview.note")}>
        {body}
      </section>
    </article>
  );
}

// --- Visão geral ---

function Overview({ projectId, invalid }: { projectId: string; invalid: boolean }) {
  const { t } = useT();
  const [state, setState] = useState<{ status: "loading" } | { status: "ok"; saved: string; max: number } | { status: "error"; failure: ApiFailure }>({ status: "loading" });
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const [savedNote, setSavedNote] = useState(false);
  const fieldId = useId();
  const hintId = useId();

  useEffect(() => {
    let live = true;
    void getKbOverview(projectId).then((result) => {
      if (!live) return;
      if (result.ok) {
        setState({ status: "ok", saved: result.data.text, max: result.data.max_bytes });
        setDraft(result.data.text);
      } else if (invalid) {
        setState({ status: "ok", saved: "", max: 32 * 1024 });
      } else {
        setState({ status: "error", failure: result.error });
      }
    });
    return () => {
      live = false;
    };
  }, [projectId, invalid]);

  if (state.status === "loading") {
    return (
      <p role="status" className="nd-muted nd-kb-note">
        {t("kb.overview.loading")}
      </p>
    );
  }
  if (state.status === "error") {
    return (
      <p role="alert" className="nd-field-error nd-kb-note">
        {apiFailureText(state.failure)}
      </p>
    );
  }
  const used = new TextEncoder().encode(draft).length;
  const kib = (n: number): string => formatNumber(n / 1024, { maximumFractionDigits: 1 });

  async function save(event: FormEvent): Promise<void> {
    event.preventDefault();
    setBusy(true);
    setSavedNote(false);
    const result = await putKbOverview(projectId, draft);
    setBusy(false);
    if (!result.ok) {
      setFailure(result.error);
      return;
    }
    setFailure(null);
    setSavedNote(true);
    setState({ status: "ok", saved: result.data.text, max: result.data.max_bytes });
    setKbOverviewFlag(projectId, result.data.text !== "");
  }

  const max = state.max;
  return (
    <form className="nd-kb-overview nd-kb-scroll" data-kb-overview="" onSubmit={(e) => void save(e)}>
      {invalid ? (
        <p className="nd-strip" data-tone="amber" role="status">
          {t("kb.overview.invalid")}
        </p>
      ) : null}
      <label htmlFor={fieldId} className="nd-section-label">
        {t("kb.tab.overview")}
      </label>
      <p id={hintId} className="nd-field-hint">
        {t("kb.overview.hint")}
      </p>
      <textarea
        id={fieldId}
        className="nd-input nd-textarea nd-kb-overview-input"
        value={draft}
        placeholder={t("kb.overview.placeholder")}
        aria-describedby={hintId}
        onChange={(e) => {
          setDraft(e.target.value);
          setSavedNote(false);
        }}
      />
      <div className="nd-row nd-wrap nd-kb-overview-foot">
        <span className="nd-muted" data-over={used > max ? "true" : undefined}>
          {t("kb.overview.usage", { used: kib(used), max: kib(max) })}
        </span>
        <span className="nd-kb-spacer" />
        {savedNote ? (
          <span role="status" className="nd-muted">
            {t("kb.overview.saved")}
          </span>
        ) : null}
        <button type="submit" className="nd-btn" data-variant="primary" disabled={busy || draft === state.saved || used > max} aria-busy={busy}>
          {busy ? t("kb.saving") : t("kb.save")}
        </button>
      </div>
      {failure !== null ? (
        <p role="alert" className="nd-field-error">
          {apiFailureText(failure)}
        </p>
      ) : null}
    </form>
  );
}

// --- Janela ---

const KnowledgeView = memo(function KnowledgeView({ projectId, size }: { projectId: string; size: ContainerSize }) {
  const { t } = useT();
  const kb = useKb(projectId);
  const [tab, setTab] = useState<Tab>("docs");
  const [selected, setSelected] = useState<string | null>(null);
  const [section, setSection] = useState<string | null>(null);
  const [showPreview, setShowPreview] = useState(false);
  const request = useOpenKbDocRequest(projectId);
  const stacked = kbArrangement(size) === "stacked";

  // Citação do Plano: abre o documento e a seção.
  useEffect(() => {
    if (request === null) return;
    setTab("docs");
    setSelected(request.doc);
    setSection(request.section);
    setShowPreview(true);
    consumeOpenKbDoc(request.id);
  }, [request]);

  const docs = kb?.status === "ok" ? kb.list.docs : [];
  const doc = docs.find((d) => d.id === selected) ?? null;
  // Sem nada escolhido, a prévia mostra o primeiro (lado a lado).
  const shown = doc ?? (stacked ? null : (docs[0] ?? null));

  const open = (id: string): void => {
    setSelected(id);
    setSection(null);
    setShowPreview(true);
  };

  const tools = (
    <WindowTools inlineOutsideWindow>
      <div className="nd-kb-toolbar" data-kb-toolbar="">
        <div role="group" aria-label={t("kb.tabs")} className="nd-segment nd-chrome">
          <button type="button" className="nd-segment-item" aria-pressed={tab === "docs"} onClick={() => setTab("docs")}>
            <span>{t("kb.tab.docs")}</span>
          </button>
          <button type="button" className="nd-segment-item" aria-pressed={tab === "overview"} onClick={() => setTab("overview")}>
            <span>{t("kb.tab.overview")}</span>
          </button>
        </div>
        <button type="button" className="nd-btn nd-iconbtn nd-btn-sm" data-variant="ghost" aria-label={t("kb.refresh")} title={t("kb.refresh")} onClick={() => void reloadKb(projectId)}>
          <Icon name="refresh" size={14} />
        </button>
      </div>
    </WindowTools>
  );

  let body: ReactNode;
  if (tab === "overview") {
    body = <Overview projectId={projectId} invalid={kb?.status === "ok" && kb.list.overview_invalid === true} />;
  } else if (kb === null || kb.status === "loading") {
    body = (
      <p role="status" className="nd-muted nd-kb-note">
        {t("kb.list.loading")}
      </p>
    );
  } else if (kb.status === "error") {
    body = (
      <p role="alert" className="nd-field-error nd-kb-note">
        {t("kb.list.error", { message: apiFailureText(kb.failure) })}
      </p>
    );
  } else if (docs.length === 0) {
    body = (
      <div className="nd-kb-empty nd-kb-scroll" data-kb-empty="">
        <Icon name="knowledge" size={20} className="nd-empty-icon" />
        <h2 className="nd-kb-empty-title">{t("kb.empty.title")}</h2>
        <p className="nd-kb-empty-text">{t("kb.empty.text")}</p>
        <AddArea projectId={projectId} empty />
        <p className="nd-muted nd-kb-private">
          <Icon name="key" size={14} />
          <span>{t("kb.private")}</span>
        </p>
      </div>
    );
  } else {
    const list = (
      <div className="nd-kb-side" data-kb-side="">
        <AddArea projectId={projectId} empty={false} />
        <ul className="nd-kb-list nd-app-scroll" aria-label={t("kb.list.label")}>
          {docs.map((d) => (
            <DocRow key={d.id} doc={d} docs={docs} selected={shown?.id === d.id} onOpen={() => open(d.id)} />
          ))}
        </ul>
      </div>
    );
    const preview =
      shown === null ? (
        <div className="nd-kb-preview nd-kb-placeholder">
          <EmptyState icon="file" message={t("kb.preview.empty")} />
        </div>
      ) : (
        <Preview
          key={shown.id}
          projectId={projectId}
          doc={shown}
          docs={docs}
          section={shown.id === selected ? section : null}
          onRemoved={() => {
            setSelected(null);
            setShowPreview(false);
          }}
        />
      );
    if (stacked) {
      body =
        showPreview && shown !== null ? (
          <div className="nd-kb-body" data-arrangement="stacked">
            <div className="nd-kb-back" data-kb-back="">
              <button type="button" className="nd-btn nd-btn-sm" onClick={() => setShowPreview(false)}>
                <Icon name="arrow-left" size={14} />
                {t("common.back")}
              </button>
              <span className="nd-kb-back-title" aria-hidden="true">
                {shown.title}
              </span>
            </div>
            {preview}
          </div>
        ) : (
          <div className="nd-kb-body" data-arrangement="stacked">
            {list}
          </div>
        );
    } else {
      body = (
        <div className="nd-kb-body" data-arrangement="split">
          {list}
          {preview}
        </div>
      );
    }
  }

  return (
    <div className="nd-kb" data-kb="" data-size={size}>
      {tools}
      {body}
    </div>
  );
});

function KnowledgeContentView({ size }: AppContentProps) {
  const space = useSpace();
  if (space === null) return null;
  return <KnowledgeView key={space.data.projectId} projectId={space.data.projectId} size={size} />;
}

export const KnowledgeContent = memo(KnowledgeContentView);
