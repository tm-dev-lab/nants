/*
 * Perguntas do Arquiteto com botões (bloco nants-choices): escolha única ou múltipla, "Outro" com campo livre, "Enviar
 * respostas" e "Decida você". Interativo só na última mensagem do Arquiteto; depois de respondido,
 * o resumo estático.
 */
import { useId, useState } from "react";
import { acceptRecommendationsText, formatAnswers, type ChoiceQuestion } from "../../choices";
import { Icon } from "../../desktop/Icon";
import { useKeyboardReveal } from "../../desktop/viewport";
import { useT } from "../../i18n/react";

interface Props {
  questions: readonly ChoiceQuestion[];
  answered: string | null;
  unanswered?: boolean;
  disabled?: boolean;
  onSubmit: (text: string) => void;
}

interface Pick {
  options: string[];
  other: boolean;
  otherText: string;
}

const EMPTY: Pick = { options: [], other: false, otherText: "" };

function answerOf(p: Pick): string[] {
  const out = [...p.options];
  if (p.other && p.otherText.trim() !== "") out.push(p.otherText.trim());
  return out;
}

export function ChoicesForm({ questions, answered, unanswered = false, disabled = false, onSubmit }: Props) {
  const uid = useId();
  const { t } = useT();
  const formRef = useKeyboardReveal("field");
  const [picks, setPicks] = useState<Pick[]>(() => questions.map((q) => ({ ...EMPTY, options: [...(q.recommended ?? [])] })));

  if (unanswered && answered === null) {
    return <p className="nd-muted nd-choices-note">{t("choices.noAnswer")}</p>;
  }
  if (answered !== null) {
    return (
      <div className="nd-choices-answered">
        <p className="nd-strong">{t("choices.yourChoices")}</p>
        <p className="nd-prewrap">{answered}</p>
      </div>
    );
  }

  const answers = picks.map(answerOf);
  const complete = answers.every((a) => a.length > 0);
  const update = (i: number, fn: (p: Pick) => Pick) => setPicks((prev) => prev.map((p, j) => (j === i ? fn(p) : p)));

  return (
    <form
      ref={formRef}
      data-keyboard-area
      className="nd-choices"
      onSubmit={(event) => {
        event.preventDefault();
        if (complete && !disabled) onSubmit(formatAnswers(questions, answers));
      }}
    >
      {questions.map((q, i) => {
        const pick = picks[i] ?? EMPTY;
        const type = q.multi ? "checkbox" : "radio";
        const name = `${uid}-${i}`;
        return (
          <fieldset key={`${q.id}-${i}`} className="nd-choices-question">
            <legend className="nd-choices-legend">
              {q.text} <span className="nd-muted">{q.multi ? t("choices.pickMany") : t("choices.pickOne")}</span>
            </legend>
            {q.why !== undefined ? <p className="nd-muted">{q.why}</p> : null}
            <div className="nd-choices-options">
              {q.options.map((opt) => {
                const on = pick.options.includes(opt);
                return (
                  <label key={opt} className="nd-option" data-checked={on ? "true" : "false"}>
                    {q.multi && on ? <Icon name="check" size={14} /> : null}
                    <input
                      type={type}
                      name={name}
                      value={opt}
                      checked={on}
                      disabled={disabled}
                      className="nd-sr-only"
                      onChange={() =>
                        update(i, (p) => ({
                          ...p,
                          options: q.multi ? (on ? p.options.filter((o) => o !== opt) : [...p.options, opt]) : [opt],
                          other: q.multi ? p.other : false,
                        }))
                      }
                    />
                    {opt}
                    {q.recommended?.includes(opt) === true ? <span className="nd-pill nd-pill-sm" data-tone="violet">{t("choices.recommended")}</span> : null}
                  </label>
                );
              })}
              <label className="nd-option" data-checked={pick.other ? "true" : "false"}>
                {q.multi && pick.other ? <Icon name="check" size={14} /> : null}
                <input
                  type={type}
                  name={name}
                  value="__other__"
                  checked={pick.other}
                  disabled={disabled}
                  className="nd-sr-only"
                  onChange={() =>
                    update(i, (p) => ({
                      ...p,
                      other: q.multi ? !p.other : true,
                      options: q.multi ? p.options : [],
                    }))
                  }
                />
                {t("choices.other")}
              </label>
            </div>
            {pick.other ? (
              <input
                type="text"
                aria-label={t("choices.otherLabel", { question: q.text })}
                value={pick.otherText}
                disabled={disabled}
                onChange={(event) => update(i, (p) => ({ ...p, otherText: event.target.value }))}
                className="nd-input"
                placeholder={t("choices.otherPlaceholder")}
              />
            ) : null}
          </fieldset>
        );
      })}
      <div className="nd-actions">
        <button type="submit" className="nd-btn" disabled={!complete || disabled}>
          {t("choices.submit")}
        </button>
        <button type="button" className="nd-btn" disabled={disabled} onClick={() => onSubmit(acceptRecommendationsText())}>
          {t("choices.decide")}
        </button>
      </div>
    </form>
  );
}
