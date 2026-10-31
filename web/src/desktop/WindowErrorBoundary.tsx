/*
 * Fronteira de erro do conteúdo de uma janela (spec 138 T13). Um erro de renderização dentro de
 * uma janela fica nela: a janela mostra o aviso com "Recarregar a janela", e a barra, as outras
 * janelas e o resto da aplicação continuam. Recarregar monta o conteúdo de novo, do zero. Vale
 * também para a janela em tela cheia do celular (a mesma moldura).
 */
import { Component, Fragment, type ErrorInfo, type ReactNode } from "react";
import { useT } from "../i18n/react";
import { Icon } from "./Icon";

function CrashedWindow({ onReload }: { onReload: () => void }) {
  const { t } = useT();
  return (
    <div className="nd-empty" data-empty-state data-window-crashed="" role="alert">
      <Icon name="alert" size={20} className="nd-empty-icon" />
      <p>{t("window.crashed")}</p>
      <button type="button" className="nd-btn" onClick={onReload}>
        {t("window.crashedReload")}
      </button>
    </div>
  );
}

interface Props {
  children: ReactNode;
}

interface State {
  failed: boolean;
  /** Muda a cada recarga: o conteúdo é montado de novo, sem o estado que quebrou. */
  generation: number;
}

export class WindowErrorBoundary extends Component<Props, State> {
  override state: State = { failed: false, generation: 0 };

  static getDerivedStateFromError(): Partial<State> {
    return { failed: true };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo): void {
    // Log técnico no console do navegador (não é texto de tela).
    console.error("window content crashed", error, info.componentStack);
  }

  private readonly reload = (): void => {
    this.setState((s) => ({ failed: false, generation: s.generation + 1 }));
  };

  override render(): ReactNode {
    if (this.state.failed) return <CrashedWindow onReload={this.reload} />;
    return <Fragment key={this.state.generation}>{this.props.children}</Fragment>;
  }
}
