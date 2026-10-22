/*
 * Avatar SVG por papel: cor pelo tom dos
 * tokens (`data-tone`), anel de estado (pensando, trabalhando, aguardando você) e o mesmo desenho.
 * Nas listas de mensagens o nome do papel já aparece ao lado, então o chamador passa
 * `decorative`; fora disso, o avatar é `role="img"` com o nome do papel.
 */
import { ICON_STROKE, type IconSize } from "../../desktop/theme/tokens";
import { agentLabel, agentTone, isKnownAgent } from "./agents";

/** Lado do desenho do avatar: da escala de ícones (094/T06). */
const GLYPH_SIZE: IconSize = 16;

function Glyph({ kind }: { kind: string }) {
  const common = {
    viewBox: "0 0 24 24",
    width: GLYPH_SIZE,
    height: GLYPH_SIZE,
    fill: "none",
    stroke: "currentColor",
    strokeWidth: ICON_STROKE,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    "aria-hidden": true,
    focusable: false,
  };
  switch (kind) {
    case "architect":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="8.5" />
          <path d="M15.2 8.8 13.4 13.4 8.8 15.2 10.6 10.6Z" />
        </svg>
      );
    case "developer":
      return (
        <svg {...common}>
          <path d="M8.5 8 4.5 12l4 4" />
          <path d="M15.5 8l4 4-4 4" />
          <path d="M13.5 6 10.5 18" />
        </svg>
      );
    case "reviewer":
      return (
        <svg {...common}>
          <circle cx="10.5" cy="10.5" r="6" />
          <path d="M15 15l5 5" />
          <path d="M7.8 10.6l1.9 1.9 3.3-3.4" />
        </svg>
      );
    case "system":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="3" />
          <circle cx="12" cy="12" r="7.5" strokeDasharray="3 2.2" />
          <circle cx="12" cy="12" r="6" />
        </svg>
      );
    case "tests":
      return (
        <svg {...common}>
          <path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3" />
        </svg>
      );
    case "user":
      return (
        <svg {...common}>
          <circle cx="12" cy="8.5" r="3.5" />
          <path d="M5 20c.6-3.4 3.4-5.5 7-5.5s6.4 2.1 7 5.5" />
        </svg>
      );
    default:
      return (
        <svg {...common}>
          <circle cx="7" cy="12" r="1" fill="currentColor" />
          <circle cx="12" cy="12" r="1" fill="currentColor" />
          <circle cx="17" cy="12" r="1" fill="currentColor" />
        </svg>
      );
  }
}

/** Estado visual do agente: pensando, trabalhando ou aguardando você. */
type AvatarState = "thinking" | "working" | "waiting";

/** Estado de uma bolha de atividade: com comando em execução trabalha; sem ele, pensa. */
export function avatarStateForActivity(activity: { command?: string | undefined }): AvatarState {
  return activity.command !== undefined ? "working" : "thinking";
}

interface Props {
  kind: string;
  decorative?: boolean;
  state?: AvatarState | undefined;
  size?: "md" | "sm";
}

export function AgentAvatar({ kind, decorative = false, state, size = "md" }: Props) {
  const known = isKnownAgent(kind);
  const a11y = decorative || state !== undefined ? { "aria-hidden": true as const } : { role: "img", "aria-label": agentLabel(known ? kind : "") };
  return (
    <span
      {...a11y}
      className="nd-avatar"
      data-kind={known ? kind : "unknown"}
      data-tone={agentTone(kind)}
      data-size={size}
      data-state={state}
    >
      <Glyph kind={known ? kind : ""} />
      {state === undefined ? null : <span aria-hidden="true" className="nd-avatar-ring" data-avatar-ring={state} />}
    </span>
  );
}
