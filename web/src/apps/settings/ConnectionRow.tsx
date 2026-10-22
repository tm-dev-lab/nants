/*
 * Linha de uma conexão salva (095/T06, `reference/Janelas.dc.html`): ponto de estado, nome,
 * detalhe em `--muted` (host, protocolo e só a máscara da chave; a URL inteira na dica), "usada
 * por …" e as ações. Nas Configurações (com `onEdit`) as ações ficam no menu "…" (Testar,
 * Editar); nos Primeiros passos, "Testar" fica na linha. O teste usa o id salvo (nenhuma chave
 * sai do navegador). Quando o provedor não lista modelos, o usuário informa um modelo para testar
 * por chat. Remover não existe: o servidor não tem rota para apagar uma conexão.
 */
import "./settings.css";
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { testConnection } from "../../api/client";
import type { ConnectionView } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { PopupMenu } from "../../desktop/PopupMenu";
import type { Tone } from "../../desktop/theme/tokens";
import { t } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { describeTestResult } from "../../settings/rules";
import { connectionDetail, connectionUsageText } from "./settingsModel";

interface TestState {
  /** Texto do resultado, montado na hora de exibir (no idioma ativo). */
  message: () => string;
  ok: boolean;
  unsupported: boolean;
}

interface ConnectionRowProps {
  connection: ConnectionView;
  /** Papéis que usam a conexão (`undefined`: a linha não mostra o uso). */
  users?: readonly string[];
  onEdit?: (connection: ConnectionView) => void;
  /** Resultado de um teste (os Primeiros passos marcam a etapa como testada). */
  onTested?: (ok: boolean) => void;
}

/** Tom do ponto: o último teste manda; sem teste, em uso (verde) ou sem uso (neutro). */
export function connectionDotTone(result: { ok: boolean } | null, users: readonly string[] | undefined): Tone | null {
  if (result !== null) return result.ok ? "green" : "rose";
  if (users !== undefined && users.length > 0) return "green";
  return null;
}

function dotLabel(result: { ok: boolean } | null, users: readonly string[] | undefined): string {
  if (result !== null) return result.ok ? t("settings.connection.testOk") : t("settings.connection.testFailed");
  if (users === undefined) return t("settings.connection.untested");
  return users.length > 0 ? t("settings.connection.inUse") : t("settings.connection.unused");
}

export function ConnectionRow({ connection, users, onEdit, onTested }: ConnectionRowProps) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<TestState | null>(null);
  const [testModel, setTestModel] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const menuButton = useRef<HTMLButtonElement | null>(null);
  const modelFieldId = useId();
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  async function runTest(model?: string): Promise<void> {
    setBusy(true);
    setResult(null);
    const outcome = await testConnection(model === undefined ? { id: connection.id } : { id: connection.id, model });
    if (!mounted.current) return;
    setBusy(false);
    if (!outcome.ok) {
      const failure = outcome.error;
      setResult({ message: () => apiFailureText(failure), ok: false, unsupported: false });
      onTested?.(false);
      return;
    }
    const ok = outcome.data.ok === true;
    const data = outcome.data;
    setResult({ message: () => describeTestResult(data), ok, unsupported: !ok && data.error === "unsupported" });
    onTested?.(ok);
  }

  const submitModelTest = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    const model = testModel.trim();
    if (model.length > 0) void runTest(model);
  };

  const closeMenu = (returnFocus: boolean): void => {
    setMenuOpen(false);
    if (returnFocus) menuButton.current?.focus({ preventScroll: true });
  };

  const tone = connectionDotTone(result, users);
  return (
    <li className="nd-conn" data-connection={connection.id} data-conn-tone={tone ?? "muted"}>
      <div className="nd-conn-row">
        <span className="nd-dot" data-tone={tone ?? undefined} aria-hidden="true" />
        <span className="nd-sr-only">{dotLabel(result, users)}</span>
        <span className="nd-conn-text">
          <span className="nd-conn-name">{connection.name}</span>
          <span className="nd-conn-detail" title={connection.base_url} data-conn-detail="">
            {connectionDetail(connection)}
          </span>
        </span>
        {users !== undefined ? (
          <span className="nd-conn-usage" data-conn-usage="">
            {connectionUsageText(users)}
          </span>
        ) : null}
        {onEdit !== undefined ? (
          <>
            <button
              ref={menuButton}
              type="button"
              className="nd-btn nd-iconbtn nd-btn-sm"
              data-variant="ghost"
              aria-label={t("settings.connection.moreActions", { name: connection.name })}
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-busy={busy}
              data-conn-menu=""
              onClick={() => setMenuOpen((v) => !v)}
            >
              <Icon name="more" size={16} />
            </button>
            {menuOpen ? (
              <PopupMenu label={t("settings.connection.actions", { name: connection.name })} anchor={menuButton} onClose={closeMenu} width={200}>
                <button
                  type="button"
                  role="menuitem"
                  className="nd-menu-item"
                  disabled={busy}
                  onClick={() => {
                    closeMenu(true);
                    void runTest();
                  }}
                >
                  <Icon name="zap" size={16} />
                  <span className="nd-menu-text">{t("settings.connection.test")}</span>
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="nd-menu-item"
                  data-connection-edit=""
                  onClick={() => {
                    closeMenu(false);
                    onEdit(connection);
                  }}
                >
                  <Icon name="pencil" size={16} />
                  <span className="nd-menu-text">{t("settings.connection.edit")}</span>
                </button>
              </PopupMenu>
            ) : null}
          </>
        ) : (
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            data-variant="ghost"
            aria-label={t("settings.connection.testNamed", { name: connection.name })}
            disabled={busy}
            aria-busy={busy}
            onClick={() => void runTest()}
          >
            <Icon name="zap" size={14} />
            {busy ? t("settings.connection.testing") : t("settings.connection.test")}
          </button>
        )}
      </div>
      {busy && onEdit !== undefined ? (
        <p role="status" className="nd-conn-note nd-muted">
          {t("settings.connection.testing")}
        </p>
      ) : null}
      {result !== null ? (
        <p role="status" className="nd-test-result nd-conn-note" data-ok={result.ok ? "true" : "false"}>
          <Icon name={result.ok ? "check" : "alert"} size={14} />
          {result.message()}
        </p>
      ) : null}
      {result?.unsupported === true ? (
        <form onSubmit={submitModelTest} className="nd-row nd-conn-note">
          <label htmlFor={modelFieldId} className="nd-field-label">
            {t("settings.connection.modelToTest")}
          </label>
          <input id={modelFieldId} className="nd-input" value={testModel} onChange={(e) => setTestModel(e.target.value)} autoComplete="off" spellCheck={false} />
          <button type="submit" className="nd-btn" aria-label={t("settings.connection.testByChatNamed", { name: connection.name })} disabled={busy || testModel.trim().length === 0}>
            {t("settings.connection.testByChat")}
          </button>
        </form>
      ) : null}
    </li>
  );
}
