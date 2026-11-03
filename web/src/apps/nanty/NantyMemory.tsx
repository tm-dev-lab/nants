/*
 * Aba Memória do Nanty (spec 140 T08, G4): as notas que ele guarda de uma conversa para a outra.
 *
 * - **Por tema**, na ordem do índice, com a procedência em linguagem de pessoa: quem disse, por qual
 *   canal, quando e de qual projeto. A que saiu do índice por falta de uso fica marcada.
 * - **Editar com versão**, como a base de conhecimento: o `PUT` leva o `updated` lido. Se a nota
 *   mudou no meio (409 `nanty_memory_conflict`), a aba relê a memória, mostra a versão gravada e a
 *   pessoa escolhe: ficar com ela ou gravar a sua por cima (agora sobre a versão nova).
 * - **Apagar é reversível** pelo prazo do servidor; as apagadas ficam no fim, com o "volta até" e o
 *   restaurar.
 * - **Runbook:** o comando exato em bloco de código, com o botão de copiar (C-23: no Telegram vai só
 *   o passo em prosa, apontando esta aba). O que o Nanty guardou sozinho leva o lembrete de conferir
 *   antes de rodar.
 * - **Ordem:** no tema, as notas em uso primeiro; as que saíram do índice vão para o fim.
 * - **Baixar uma cópia:** `POST /api/nanty/memory/export`, gravada como arquivo JSON pelo navegador.
 *
 * O texto de cada nota é sempre texto (nós de texto do React), nunca HTML. A aba relê a memória
 * quando a conversa muda (o servidor avisa a memória pela mesma parte), sem polling.
 */
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { Icon } from "../../desktop/Icon";
import { formatDateTime, type MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { useAppStore } from "../../store";
import { exportNantyMemory, getNantyMemory, postNantyMemory, putNantyMemory, type MemoryEntry, type MemoryTopic, type MemoryView } from "./nantyMemoryApi";
import { memoryLayout, runbookParts, textMax, topicWritable } from "./nantyMemoryModel";
import { useNantyConversation } from "./nantyStore";

const TOPIC_KEYS: Readonly<Record<MemoryTopic, MessageKey>> = {
  identity: "nanty.memoryTab.topic.identity",
  working_together: "nanty.memoryTab.topic.working_together",
  system: "nanty.memoryTab.topic.system",
  runbooks: "nanty.memoryTab.topic.runbooks",
  decisions: "nanty.memoryTab.topic.decisions",
};

const WHO_KEYS: Readonly<Record<MemoryEntry["source"]["kind"], MessageKey>> = {
  person_said: "nanty.memoryTab.who.person_said",
  model_clean: "nanty.memoryTab.who.model_clean",
  person_confirmed: "nanty.memoryTab.who.person_confirmed",
  imported: "nanty.memoryTab.who.imported",
  unknown: "nanty.memoryTab.who.unknown",
};

type Notice = { tone: "ok" | "warn"; text: string };

/** A edição aberta: o rascunho, a versão em que ele partiu e, se houve conflito, a gravada agora. */
interface Draft {
  id: string;
  topic: MemoryTopic;
  title: string;
  text: string;
  base: number;
  conflict: MemoryEntry | "gone" | null;
}

/** Nome do projeto pelo id (a lista do Nants); sem cadastro, nada. */
function useProjectName(): (id: string | null) => string | null {
  const projects = useAppStore((s) => s.config?.projects);
  return (id) => (id === null ? null : (projects?.find((p) => p.id === id)?.name ?? null));
}

/** A procedência numa linha: quem, por onde, quando e o projeto. */
function SourceLine({ entry }: { entry: MemoryEntry }) {
  const { t } = useT();
  const projectName = useProjectName();
  const who = t(WHO_KEYS[entry.source.kind]);
  const when = formatDateTime(entry.source.at_ms);
  const said =
    entry.source.channel === null
      ? t("nanty.memoryTab.sourceNoChannel", { who, when })
      : t("nanty.memoryTab.source", { who, where: t(entry.source.channel === "web" ? "nanty.memoryTab.where.web" : "nanty.memoryTab.where.telegram"), when });
  const project = projectName(entry.source.project_id);
  return (
    <p className="nd-nanty-facts" data-memory-source={entry.source.kind}>
      {project === null ? said : `${said} · ${t("nanty.memoryTab.project", { name: project })}`}
    </p>
  );
}

/** Copia o comando (sem rede; sem área de transferência vira "Copie manualmente"). */
function CopyCode({ text, label }: { text: string; label: string }) {
  const { t } = useT();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <button
      type="button"
      className="nd-btn nd-btn-sm"
      data-variant="ghost"
      aria-label={t("common.copyCommandFor", { label })}
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

/** O texto da nota: o runbook em prosa e blocos de código; os outros temas, o texto como está. */
function NoteBody({ entry }: { entry: MemoryEntry }) {
  const { t } = useT();
  if (entry.topic !== "runbooks") {
    return <p className="nd-nanty-memory-text">{entry.text}</p>;
  }
  return (
    <div className="nd-nanty-memory-runbook">
      {runbookParts(entry.text).map((part, i) =>
        part.kind === "text" ? (
          <p key={i} className="nd-nanty-memory-text">
            {part.text}
          </p>
        ) : (
          <div key={i} className="nd-nanty-memory-code" data-memory-code="">
            <pre className="nd-nanty-memory-pre">
              <code>{part.text}</code>
            </pre>
            <div className="nd-nanty-memory-code-side">
              <CopyCode text={part.text} label={entry.title} />
              {entry.source.kind === "model_clean" ? (
                <span className="nd-nanty-memory-check" data-memory-check="">
                  {t("nanty.memoryTab.checkFirst")}
                </span>
              ) : null}
            </div>
          </div>
        ),
      )}
    </div>
  );
}

interface EditorProps {
  draft: Draft;
  view: MemoryView;
  busy: boolean;
  onChange: (draft: Draft) => void;
  onSave: (draft: Draft) => void;
  onCancel: () => void;
}

function NoteEditor({ draft, view, busy, onChange, onSave, onCancel }: EditorProps) {
  const { t } = useT();
  const base = useId();
  const titleMax = view.limits.title_max_chars;
  const max = textMax(draft.topic, view.limits);
  const titleLen = [...draft.title.trim()].length;
  const textLen = [...draft.text.trim()].length;
  const invalid = titleLen === 0 || textLen === 0 || titleLen > titleMax || textLen > max;
  const conflict = draft.conflict;
  return (
    <form
      className="nd-nanty-memory-form"
      data-memory-editor={draft.id}
      onSubmit={(e) => {
        e.preventDefault();
        if (!invalid && !busy && conflict === null) onSave(draft);
      }}
    >
      <div className="nd-field">
        <label className="nd-field-label" htmlFor={`${base}-title`}>
          {t("nanty.memoryTab.field.title")}
        </label>
        <input id={`${base}-title`} className="nd-input" value={draft.title} disabled={busy} onChange={(e) => onChange({ ...draft, title: e.target.value })} />
        <span className="nd-field-hint nd-num" data-over={titleLen > titleMax ? "true" : undefined}>
          {t("nanty.memoryTab.used", { used: titleLen, max: titleMax })}
        </span>
      </div>
      <div className="nd-field">
        <label className="nd-field-label" htmlFor={`${base}-text`}>
          {t("nanty.memoryTab.field.text")}
        </label>
        <textarea
          id={`${base}-text`}
          className="nd-input nd-textarea nd-nanty-memory-input"
          value={draft.text}
          disabled={busy}
          onChange={(e) => onChange({ ...draft, text: e.target.value })}
        />
        <span className="nd-field-hint nd-num" data-over={textLen > max ? "true" : undefined}>
          {t("nanty.memoryTab.used", { used: textLen, max })}
        </span>
      </div>
      {conflict === "gone" ? (
        <p role="alert" className="nd-nanty-notice" data-tone="warn" data-memory-gone="">
          {t("nanty.memoryTab.conflict.gone")}
        </p>
      ) : conflict !== null ? (
        <div role="alert" className="nd-nanty-memory-conflict" data-memory-conflict="">
          <strong className="nd-nanty-memory-conflict-title">{t("nanty.memoryTab.conflict.title")}</strong>
          <span className="nd-nanty-memory-conflict-text">{t("nanty.memoryTab.conflict.text")}</span>
          <div className="nd-nanty-memory-current" data-memory-current="">
            <span className="nd-nanty-memory-title">{conflict.title}</span>
            <NoteBody entry={conflict} />
            <SourceLine entry={conflict} />
          </div>
          <div className="nd-row nd-wrap">
            <button type="button" className="nd-btn nd-btn-sm" disabled={busy} onClick={onCancel}>
              <Icon name="refresh" size={14} />
              {t("nanty.memoryTab.conflict.theirs")}
            </button>
            <button type="button" className="nd-btn nd-btn-sm" disabled={busy || invalid} onClick={() => onSave({ ...draft, base: conflict.updated, conflict: null })}>
              <Icon name="save" size={14} />
              {t("nanty.memoryTab.conflict.mine")}
            </button>
          </div>
        </div>
      ) : null}
      {conflict === null ? (
        <div className="nd-row nd-wrap">
          <button type="submit" className="nd-btn nd-btn-sm" data-variant="primary" disabled={busy || invalid}>
            <Icon name="save" size={14} />
            {t("nanty.memoryTab.save")}
          </button>
          <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" disabled={busy} onClick={onCancel}>
            {t("nanty.memoryTab.cancel")}
          </button>
        </div>
      ) : conflict === "gone" ? (
        <div className="nd-row nd-wrap">
          <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" onClick={onCancel}>
            {t("nanty.memoryTab.cancel")}
          </button>
        </div>
      ) : null}
    </form>
  );
}

interface NoteProps {
  entry: MemoryEntry;
  writable: boolean;
  busy: boolean;
  onEdit: () => void;
  onDelete: () => void;
}

function Note({ entry, writable, busy, onEdit, onDelete }: NoteProps) {
  const { t } = useT();
  return (
    <article className="nd-nanty-memory-note" data-memory-entry={entry.id} data-stale={entry.stale ? "true" : undefined}>
      <div className="nd-nanty-memory-head">
        <h5 className="nd-nanty-memory-title">{entry.title}</h5>
        {writable ? (
          <div className="nd-row nd-nanty-memory-actions">
            <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" disabled={busy} aria-label={t("nanty.memoryTab.editLabel", { title: entry.title })} onClick={onEdit}>
              <Icon name="pencil" size={14} />
              {t("nanty.memoryTab.edit")}
            </button>
            <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" disabled={busy} aria-label={t("nanty.memoryTab.deleteLabel", { title: entry.title })} onClick={onDelete}>
              <Icon name="trash" size={14} />
              {t("nanty.memoryTab.delete")}
            </button>
          </div>
        ) : null}
      </div>
      <NoteBody entry={entry} />
      <SourceLine entry={entry} />
      {entry.stale ? (
        <p className="nd-nanty-memory-stale" data-memory-stale="">
          {t("nanty.memoryTab.stale")}
        </p>
      ) : null}
    </article>
  );
}

function DeletedNote({ entry, writable, busy, onRestore }: { entry: MemoryEntry; writable: boolean; busy: boolean; onRestore: () => void }) {
  const { t } = useT();
  return (
    <article className="nd-nanty-memory-note" data-memory-deleted={entry.id}>
      <div className="nd-nanty-memory-head">
        <h5 className="nd-nanty-memory-title">{entry.title}</h5>
        {writable ? (
          <button type="button" className="nd-btn nd-btn-sm" disabled={busy} aria-label={t("nanty.memoryTab.restoreLabel", { title: entry.title })} onClick={onRestore}>
            <Icon name="undo" size={14} />
            {t("nanty.memoryTab.restore")}
          </button>
        ) : null}
      </div>
      <p className="nd-nanty-memory-text" data-muted="true">
        {entry.text}
      </p>
      <p className="nd-nanty-facts">
        {[t(TOPIC_KEYS[entry.topic]), entry.restore_until_ms !== null ? t("nanty.memoryTab.until", { date: formatDateTime(entry.restore_until_ms) }) : null]
          .filter((p) => p !== null)
          .join(" · ")}
      </p>
    </article>
  );
}

/** Baixa o JSON como arquivo. `false` se o navegador não deixa. */
function download(data: unknown): boolean {
  if (typeof URL.createObjectURL !== "function") return false;
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `nanty-memory-${new Date().toISOString().slice(0, 10)}.json`;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
  return true;
}

export function NantyMemory() {
  const { t, tp } = useT();
  const headingId = useId();
  const conversation = useNantyConversation();
  const [view, setView] = useState<MemoryView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);
  const seq = useRef(0);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  /** Relê a memória; devolve a vista nova (a da última leitura pedida vence). */
  const load = useCallback(async (): Promise<MemoryView | null> => {
    seq.current += 1;
    const mine = seq.current;
    const result = await getNantyMemory();
    if (!alive.current || mine !== seq.current) return null;
    if (result.ok) {
      setView(result.data);
      setLoadError(null);
      return result.data;
    }
    setLoadError(t("nanty.memoryTab.loadFailed", { error: apiFailureText(result.error) }));
    return null;
  }, [t]);

  // Na abertura e a cada mudança da conversa (a memória muda junto, pela mesma parte do aviso).
  useEffect(() => {
    void load();
  }, [conversation, load]);

  const save = async (next: Draft): Promise<void> => {
    setBusy(true);
    setNotice(null);
    const result = await putNantyMemory(next.id, next.title.trim(), next.text.trim(), next.base);
    if (!alive.current) return;
    if (result.ok) {
      setDraft(null);
      setNotice({ tone: "ok", text: t("nanty.memoryTab.saved", { title: result.data.title }) });
      await load();
    } else if (result.error.code === "nanty_memory_conflict" || result.error.code === "nanty_memory_not_found") {
      // Leitura própria, fora do descarte por `seq` de `load`: a releitura que o aviso da conversa
      // dispara junto não pode apagar esta. "gone" só com uma leitura bem-sucedida sem a nota ativa.
      const fresh = await getNantyMemory();
      if (!alive.current) return;
      if (fresh.ok) {
        seq.current += 1;
        setView(fresh.data);
        setLoadError(null);
        const current = fresh.data.topics.flatMap((topic) => topic.entries).find((e) => e.id === next.id && e.state === "active");
        setDraft({ ...next, conflict: current ?? "gone" });
      } else {
        setDraft({ ...next, conflict: null });
        setNotice({ tone: "warn", text: t("nanty.memoryTab.conflict.readFailed", { error: apiFailureText(fresh.error) }) });
      }
    } else {
      setNotice({ tone: "warn", text: t("nanty.memoryTab.failed", { error: apiFailureText(result.error) }) });
    }
    setBusy(false);
  };

  const change = async (entry: MemoryEntry, action: "delete" | "restore"): Promise<void> => {
    setBusy(true);
    setNotice(null);
    const result = await postNantyMemory(entry.id, action);
    if (!alive.current) return;
    if (result.ok) {
      const days = view?.limits.deleted_keep_days ?? 1;
      setNotice({
        tone: "ok",
        text: action === "delete" ? tp("nanty.memoryTab.deleted", days, { title: entry.title }) : t("nanty.memoryTab.restored", { title: entry.title }),
      });
      if (draft?.id === entry.id) setDraft(null);
    } else {
      setNotice({ tone: "warn", text: t("nanty.memoryTab.failed", { error: apiFailureText(result.error) }) });
    }
    await load();
    setBusy(false);
  };

  const exportCopy = async (): Promise<void> => {
    setBusy(true);
    setNotice(null);
    const result = await exportNantyMemory();
    if (!alive.current) return;
    if (result.ok && download(result.data)) setNotice({ tone: "ok", text: t("nanty.memoryTab.exported") });
    else if (!result.ok) setNotice({ tone: "warn", text: t("nanty.memoryTab.exportFailed", { error: apiFailureText(result.error) }) });
    else setNotice({ tone: "warn", text: t("nanty.memoryTab.exportFailed", { error: t("api.invalidResponse") }) });
    setBusy(false);
  };

  const layout = view === null ? null : memoryLayout(view);
  const empty = layout !== null && layout.sections.length === 0 && layout.deleted.length === 0;

  return (
    <div className="nd-nanty-memory" data-nanty-memory="" aria-labelledby={headingId} role="region">
      <header className="nd-nanty-rules-head">
        <div className="nd-nanty-memory-top">
          <h3 id={headingId} className="nd-nanty-heading">
            {t("nanty.memoryTab.heading")}
          </h3>
          <button type="button" className="nd-btn nd-btn-sm" disabled={busy || view === null} onClick={() => void exportCopy()} data-memory-export="">
            <Icon name="save" size={14} />
            {t("nanty.memoryTab.export")}
          </button>
        </div>
        <p className="nd-nanty-lede">{tp("nanty.memoryTab.intro", view?.limits.deleted_keep_days ?? 7)}</p>
      </header>
      {notice !== null ? (
        <p role="status" className="nd-nanty-notice" data-tone={notice.tone} data-nanty-notice="">
          {notice.text}
        </p>
      ) : null}
      {loadError !== null ? (
        <div className="nd-row nd-wrap" data-memory-error="">
          <p role="alert" className="nd-field-error">
            {loadError}
          </p>
          <button type="button" className="nd-btn nd-btn-sm" onClick={() => void load()}>
            <Icon name="refresh" size={14} />
            {t("nanty.memoryTab.retry")}
          </button>
        </div>
      ) : null}
      {view === null && loadError === null ? <p className="nd-nanty-lede">{t("nanty.memoryTab.loading")}</p> : null}
      {view !== null && !view.writable ? (
        <p className="nd-nanty-notice" data-tone="warn" data-memory-readonly="">
          {t("nanty.memoryTab.readOnly")}
        </p>
      ) : null}
      {view !== null && draft !== null && draft.conflict === "gone" ? (
        <NoteEditor draft={draft} view={view} busy={busy} onChange={setDraft} onSave={(d) => void save(d)} onCancel={() => setDraft(null)} />
      ) : null}
      {empty ? (
        <div className="nd-nanty-empty" data-nanty-empty="memory">
          <h4 className="nd-nanty-empty-title">{t("nanty.memoryTab.empty.title")}</h4>
          <p className="nd-nanty-empty-text">{t("nanty.memoryTab.empty.text")}</p>
        </div>
      ) : null}
      {view !== null && layout !== null
        ? layout.sections.map((section) => {
            const writable = topicWritable(view, section.topic);
            return (
              <section key={section.topic} className="nd-nanty-section" data-memory-topic={section.topic}>
                <h4 className="nd-nanty-section-label">{t(TOPIC_KEYS[section.topic])}</h4>
                {section.status === "newer" || section.status === "invalid" ? (
                  <p className="nd-nanty-notice" data-tone="warn" data-memory-file={section.status}>
                    {t(section.status === "newer" ? "nanty.memoryTab.topicNewer" : "nanty.memoryTab.topicInvalid")}
                  </p>
                ) : null}
                {section.entries.map((entry) =>
                  draft?.id === entry.id && draft.conflict !== "gone" ? (
                    <NoteEditor key={entry.id} draft={draft} view={view} busy={busy} onChange={setDraft} onSave={(d) => void save(d)} onCancel={() => setDraft(null)} />
                  ) : (
                    <Note
                      key={entry.id}
                      entry={entry}
                      writable={writable && entry.state === "active"}
                      busy={busy || draft !== null}
                      onEdit={() => setDraft({ id: entry.id, topic: entry.topic, title: entry.title, text: entry.text, base: entry.updated, conflict: null })}
                      onDelete={() => void change(entry, "delete")}
                    />
                  ),
                )}
              </section>
            );
          })
        : null}
      {view !== null && layout !== null && layout.deleted.length > 0 ? (
        <section className="nd-nanty-section" data-memory-topic="deleted">
          <h4 className="nd-nanty-section-label">{t("nanty.memoryTab.deletedHeading")}</h4>
          {layout.deleted.map((entry) => (
            <DeletedNote key={entry.id} entry={entry} writable={topicWritable(view, entry.topic)} busy={busy} onRestore={() => void change(entry, "restore")} />
          ))}
        </section>
      ) : null}
    </div>
  );
}
