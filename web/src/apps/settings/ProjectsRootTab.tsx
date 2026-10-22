/*
 * Aba Projetos (spec 093, decisão 16): a raiz da pasta de projetos gerenciada pelo Nants. Padrão
 * `~/NantsProjects` (visível, para abrir no editor e no explorador do Windows via WSL). O servidor
 * valida (caminho absoluto depois de expandir `~`, nem diretório de sistema nem dentro de um
 * projeto registrado) e responde com mensagens fixas. Mudar a raiz não move projetos existentes.
 */
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { parseProjectsRoot, putProjectsRoot } from "../../desktop/api";
import { Icon } from "../../desktop/Icon";
import { t as translateNow } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { appStore, useAppStore } from "../../store";

const DEFAULT_ROOT_LABEL = "~/NantsProjects";

export function ProjectsRootTab() {
  const config = useAppStore((s) => s.config);
  const current = parseProjectsRoot(config);
  const fieldId = useId();
  const hintId = useId();
  const [value, setValue] = useState(current.projects_root ?? "");
  const [busy, setBusy] = useState(false);
  const { t } = useT();
  // Mensagem montada na hora de exibir (no idioma ativo): a falha guardada como veio.
  const [message, setMessage] = useState<{ ok: boolean; text: () => string } | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  async function save(root: string | null): Promise<void> {
    setBusy(true);
    setMessage(null);
    const result = await putProjectsRoot(root);
    if (!alive.current) return;
    setBusy(false);
    if (!result.ok) {
      const failure = result.error;
      setMessage({ ok: false, text: () => apiFailureText(failure) });
      return;
    }
    const data = result.data;
    appStore.setState((s) => (s.config === null ? s : { config: { ...s.config, ...data } }));
    setValue(data.projects_root ?? "");
    const key = root === null ? "settings.root.backToDefault" : "settings.root.saved";
    setMessage({ ok: true, text: () => translateNow(key) });
  }

  const submit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    const trimmed = value.trim();
    void save(trimmed === "" ? null : trimmed);
  };

  return (
    <form className="nd-form" onSubmit={submit} data-projects-root="" aria-label={t("settings.root.form")}>
      <p className="nd-field-hint">{t("settings.root.intro")}</p>
      <dl className="nd-facts">
        <dt>{t("settings.root.inUse")}</dt>
        <dd className="nd-mono" data-root-effective="">
          {current.projects_root_effective ?? DEFAULT_ROOT_LABEL}
        </dd>
      </dl>
      <div className="nd-field">
        <label htmlFor={fieldId} className="nd-field-label">
          {t("settings.root.field")}
        </label>
        <input
          id={fieldId}
          className="nd-input nd-mono"
          value={value}
          placeholder={DEFAULT_ROOT_LABEL}
          onChange={(e) => setValue(e.target.value)}
          aria-describedby={hintId}
          autoComplete="off"
          spellCheck={false}
        />
        <p id={hintId} className="nd-field-hint">
          {t("settings.root.hint", { default: DEFAULT_ROOT_LABEL })}
        </p>
      </div>
      {message !== null ? (
        <p role={message.ok ? "status" : "alert"} className={message.ok ? "nd-ok" : "nd-field-error"}>
          {message.ok ? <Icon name="check" size={14} /> : null}
          {message.text()}
        </p>
      ) : null}
      <div className="nd-dialog-actions">
        <button type="button" className="nd-btn" disabled={busy || current.projects_root === null} onClick={() => void save(null)}>
          <Icon name="undo" size={14} />
          {t("settings.root.useDefault")}
        </button>
        <button type="submit" className="nd-btn" data-variant="primary" disabled={busy} aria-busy={busy}>
          <Icon name="save" size={14} />
          {t("settings.root.save")}
        </button>
      </div>
    </form>
  );
}
