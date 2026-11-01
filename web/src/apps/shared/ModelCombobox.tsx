/*
 * Combobox de modelo (saiu do `AgentsForm` na spec 139 T09): o texto livre é sempre aceito e a lista
 * filtra pela digitação. Serve aos Agentes e aos Ajustes do Nanty (modelo, reserva, fala, transcrição
 * e a voz, com a lista de vozes).
 */
import { useId, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useT } from "../../i18n/react";

/** Opção da lista: `id` é o que se grava e envia; `name` (plano ChatGPT, spec 116) é o rótulo. */
export interface ModelOption {
  id: string;
  name?: string;
}

export function ModelCombobox({
  id,
  value,
  options,
  disabled,
  onChange,
  label,
  listLabel,
  placeholder,
}: {
  id?: string;
  value: string;
  options: readonly ModelOption[];
  disabled: boolean;
  onChange: (model: string) => void;
  /** Nome do campo quando ele não tem um `<label>` (a reserva). */
  label?: string;
  /** Nome da lista; sem ele, "Modelos". */
  listLabel?: string;
  placeholder?: string;
}) {
  const { t } = useT();
  const labelOf = (option: ModelOption): string => option.name ?? option.id;
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [typed, setTyped] = useState("");
  const query = typed.trim().toLowerCase();
  const filtered = query === "" ? [...options] : options.filter((m) => m.id.toLowerCase().includes(query) || labelOf(m).toLowerCase().includes(query));
  const visible = open && !disabled && filtered.length > 0;
  const current = visible && active < filtered.length ? active : -1;
  const optionId = (index: number): string => `${listId}-opt-${index}`;

  const close = (): void => {
    setOpen(false);
    setActive(-1);
    setTyped("");
  };
  const choose = (model: string): void => {
    onChange(model);
    close();
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>): void => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      if (filtered.length === 0) return;
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      if (!visible) {
        setOpen(true);
        setTyped("");
        setActive(delta > 0 ? 0 : filtered.length - 1);
        return;
      }
      const base = current < 0 ? (delta > 0 ? -1 : filtered.length) : current;
      setActive((base + delta + filtered.length) % filtered.length);
    } else if (event.key === "Enter") {
      if (visible) {
        event.preventDefault();
        if (current >= 0) choose(filtered[current]?.id ?? value);
        else close();
      }
    } else if (event.key === "Escape") {
      if (visible) {
        event.preventDefault();
        event.stopPropagation();
        close();
      }
    }
  };

  return (
    <div className="nd-combo">
      <input
        id={id}
        type="text"
        role="combobox"
        className="nd-input"
        aria-label={label}
        aria-autocomplete="list"
        aria-expanded={visible}
        aria-controls={visible ? listId : undefined}
        aria-activedescendant={current >= 0 ? optionId(current) : undefined}
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        onChange={(event) => {
          onChange(event.target.value);
          setOpen(true);
          setActive(-1);
          setTyped(event.target.value);
        }}
        onClick={() => {
          setOpen(true);
          setTyped("");
        }}
        onBlur={close}
        onKeyDown={onKeyDown}
        autoComplete="off"
        spellCheck={false}
      />
      {visible ? (
        <ul id={listId} role="listbox" aria-label={listLabel ?? t("settings.agents.models")} className="nd-combo-list">
          {filtered.map((model, index) => (
            <li
              key={model.id}
              id={optionId(index)}
              role="option"
              aria-selected={model.id === value}
              data-active={index === current ? "true" : undefined}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(model.id)}
            >
              {labelOf(model)}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
