/*
 * Escolhas de aparência (spec 093, decisão 13): tema Escuro, Claro ou Automático e o papel de
 * parede, com miniaturas no tema efetivo. Usadas pelo Personalizar e pela aba Aparência das
 * Configurações (T08), que ficam iguais.
 */
import { useId } from "react";
import { useT } from "../i18n/react";
import { Icon, type IconName } from "./Icon";
import type { Appearance } from "./theme/appearance";
import {
  THEME_LABELS,
  THEME_PREFERENCES,
  WALLPAPER_IDS,
  WALLPAPER_LABELS,
  wallpaperStyle,
  type ThemeName,
  type ThemePreference,
} from "./theme/tokens";

const THEME_ICONS: Readonly<Record<ThemePreference, IconName>> = {
  dark: "moon",
  light: "sun",
  auto: "monitor",
};

interface AppearanceChoicesProps {
  appearance: Appearance;
  theme: ThemeName;
  onChange: (next: Appearance) => void;
}

export function AppearanceChoices({ appearance, theme, onChange }: AppearanceChoicesProps) {
  const { t } = useT();
  const themeId = useId();
  const wallpaperId = useId();
  return (
    <>
      <div role="group" aria-labelledby={themeId}>
        <div id={themeId} className="nd-label">
          {t("appearance.theme")}
        </div>
        <div className="nd-choice-grid">
          {THEME_PREFERENCES.map((pref) => {
            const on = appearance.theme === pref;
            return (
              <button
                key={pref}
                type="button"
                className="nd-choice"
                aria-pressed={on}
                data-theme-choice={pref}
                onClick={() => onChange({ ...appearance, theme: pref })}
              >
                <span className="nd-choice-row">
                  <Icon name={THEME_ICONS[pref]} size={16} />
                  {THEME_LABELS[pref]}
                  {on ? <Icon name="check" size={14} className="nd-choice-check" /> : null}
                </span>
              </button>
            );
          })}
        </div>
      </div>
      <div role="group" aria-labelledby={wallpaperId}>
        <div id={wallpaperId} className="nd-label">
          {t("appearance.wallpaper")}
        </div>
        <div className="nd-choice-grid" data-columns="2">
          {WALLPAPER_IDS.map((id) => {
            const on = appearance.wallpaper === id;
            return (
              <button
                key={id}
                type="button"
                className="nd-choice"
                aria-pressed={on}
                data-wallpaper-choice={id}
                onClick={() => onChange({ ...appearance, wallpaper: id })}
              >
                <span className="nd-thumb" aria-hidden="true" style={wallpaperStyle(id, theme)} />
                <span className="nd-choice-row">
                  {WALLPAPER_LABELS[id]}
                  {on ? <Icon name="check" size={14} className="nd-choice-check" /> : null}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </>
  );
}
