/*
 * Boas-vindas (spec 093, T05): sem espaço aberto, o centro da área de trabalho mostra a marca, o
 * slogan (`BRAND_SLOGAN`) logo depois dela, a linha de produto (`BRAND_PRODUCT_LINE_KEY`, spec 100),
 * "Novo projeto", "Primeiros passos" e os projetos já existentes.
 * Também os cartões de "carregando" e "projeto não encontrado" de um espaço.
 */
import type { ProjectEntry } from "../api/types";
import { useT } from "../i18n/react";
import { BRAND_NAME, BRAND_PRODUCT_LINE_KEY, BRAND_SLOGAN } from "./brand";
import { Icon } from "./Icon";
import { NantsSymbol } from "./topbar/BrandMark";

interface WelcomeProps {
  projects: readonly ProjectEntry[];
  onNewProject: () => void;
  onOpenSetup: () => void;
  onOpenProject: (projectId: string) => void;
}

export function Welcome({ projects, onNewProject, onOpenSetup, onOpenProject }: WelcomeProps) {
  const { t } = useT();
  return (
    <section className="nd-desktop-empty nd-card nd-welcome" aria-labelledby="nd-welcome-title" data-welcome="">
      <div className="nd-welcome-brand">
        <NantsSymbol size={36} />
        <h2 id="nd-welcome-title">{BRAND_NAME}</h2>
      </div>
      <p className="nd-welcome-slogan" data-welcome-slogan="">
        {BRAND_SLOGAN}
      </p>
      <p className="nd-welcome-product" data-welcome-product="">
        {t(BRAND_PRODUCT_LINE_KEY)}
      </p>
      <p>{t("welcome.intro")}</p>
      <div className="nd-actions">
        <button type="button" className="nd-btn" data-variant="primary" onClick={onNewProject}>
          <Icon name="plus" size={14} />
          {t("common.newProject")}
        </button>
        <button type="button" className="nd-btn" onClick={onOpenSetup}>
          <Icon name="setup" size={14} />
          {t("window.gettingStarted")}
        </button>
      </div>
      {projects.length > 0 ? (
        <div className="nd-welcome-projects">
          <h3 className="nd-label">{t("welcome.yourProjects")}</h3>
          <ul aria-label={t("welcome.yourProjects")}>
            {projects.map((p) => (
              <li key={p.id}>
                <button type="button" className="nd-menu-item" onClick={() => onOpenProject(p.id)}>
                  <Icon name="project" size={16} />
                  <span className="nd-menu-text">{p.name}</span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </section>
  );
}

export function SpaceLoading() {
  const { t } = useT();
  return (
    <section className="nd-desktop-empty nd-card" data-space-loading="">
      <p role="status">{t("welcome.loadingProject")}</p>
    </section>
  );
}

export function SpaceNotFound({ onBack }: { onBack: () => void }) {
  const { t } = useT();
  return (
    <section className="nd-desktop-empty nd-card" data-space-not-found="">
      <p role="alert">{t("welcome.projectNotFound")}</p>
      <div className="nd-actions">
        <button type="button" className="nd-btn" onClick={onBack}>
          {t("welcome.backToStart")}
        </button>
      </div>
    </section>
  );
}
