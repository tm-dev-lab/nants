/*
 * Avatar do Nanty (spec 138 T04, `reference/Avatar.dc.html`): uma cara de formiga feita das formas
 * arredondadas do símbolo N, nas três cores da marca (`NANTY`, dos tokens). As antenas são a
 * assinatura: o estado aparece pela postura e pelo movimento delas. Cinco estados, todos animados:
 * - atento: antenas balançando devagar, pontas pulsando e uma piscada de vez em quando;
 * - falando: antenas agitadas e a boca abrindo e fechando;
 * - pensando: antenas oscilando juntas e o olhar passeando;
 * - dormindo: respiração lenta, antenas caídas e um "z" subindo;
 * - esperando você: antenas saltando e um ponto âmbar com uma onda.
 *
 * Movimento: as animações moram no `nanty.css` e só valem com `data-motion="live"`. Com
 * `prefers-reduced-motion` (lido pelo `animation.ts` e acompanhado ao vivo), o avatar fica parado
 * na pose do estado, que já é o desenho de cada um. A espessura dos traços também fica no CSS.
 */
import "./nanty.css";
import { useSyncExternalStore, type JSX } from "react";
import { prefersReducedMotion, subscribeReducedMotion } from "../../desktop/animation";
import { NANTY } from "../../desktop/theme/tokens";

export type NantyMood = "attentive" | "speaking" | "thinking" | "sleeping" | "waiting";

export const NANTY_MOODS: readonly NantyMood[] = ["attentive", "speaking", "thinking", "sleeping", "waiting"];

/** `prefers-reduced-motion`, acompanhado ao vivo. */
function useReducedMotion(): boolean {
  return useSyncExternalStore(
    (listener) => subscribeReducedMotion(listener),
    () => prefersReducedMotion(),
    () => false,
  );
}

const LEFT_TIP = { cx: 10, cy: 7 };
const RIGHT_TIP = { cx: 54, cy: 7 };
const TIP_R = 4.5;

/** Cabeça: o mesmo retângulo arredondado em todos os estados. */
function Head() {
  return <rect x="12" y="20" width="40" height="38" rx="18" fill={NANTY.head} />;
}

/** Olhos abertos (com o brilho, a menos que `shine` seja falso) e a piscada em `blink`. */
function Eyes({ cy, rx = 4, ry = 5, shineY, shineR = 1.4, shineX = 1.4, blink = true }: { cy: number; rx?: number; ry?: number; shineY: number; shineR?: number; shineX?: number; blink?: boolean }) {
  const eyes = (
    <>
      <ellipse cx="25" cy={cy} rx={rx} ry={ry} fill={NANTY.feature} />
      <ellipse cx="39" cy={cy} rx={rx} ry={ry} fill={NANTY.feature} />
    </>
  );
  const shine = (
    <>
      <circle cx={25 + shineX} cy={shineY} r={shineR} fill={NANTY.shine} />
      <circle cx={39 + shineX} cy={shineY} r={shineR} fill={NANTY.shine} />
    </>
  );
  return blink ? (
    <g className="nd-nanty-blink">
      {eyes}
      {shine}
    </g>
  ) : (
    <>
      {eyes}
      <g className="nd-nanty-look">{shine}</g>
    </>
  );
}

function Attentive() {
  return (
    <>
      <g className="nd-nanty-sway" data-side="left">
        <path className="nd-nanty-antenna" d="M25 22 C22 12 17 8 10 7" stroke={NANTY.antennaLeft} />
        <circle className="nd-nanty-tip" {...LEFT_TIP} r={TIP_R} fill={NANTY.antennaLeft} />
      </g>
      <g className="nd-nanty-sway" data-side="right">
        <path className="nd-nanty-antenna" d="M39 22 C42 12 47 8 54 7" stroke={NANTY.antennaRight} />
        <circle className="nd-nanty-tip" {...RIGHT_TIP} r={TIP_R} fill={NANTY.antennaRight} />
      </g>
      <Head />
      <Eyes cy={38} shineY={36} />
      <path className="nd-nanty-line" d="M27 49 Q32 52.5 37 49" stroke={NANTY.feature} />
    </>
  );
}

function Speaking() {
  return (
    <>
      <g className="nd-nanty-wiggle" data-side="left">
        <path className="nd-nanty-antenna" d="M25 22 C22 12 17 8 10 7" stroke={NANTY.antennaLeft} />
        <circle {...LEFT_TIP} r={TIP_R} fill={NANTY.antennaLeft} />
      </g>
      <g className="nd-nanty-wiggle" data-side="right">
        <path className="nd-nanty-antenna" d="M39 22 C42 12 47 8 54 7" stroke={NANTY.antennaRight} />
        <circle {...RIGHT_TIP} r={TIP_R} fill={NANTY.antennaRight} />
      </g>
      <Head />
      <Eyes cy={37} shineY={35} />
      <ellipse className="nd-nanty-talk" cx="32" cy="49.5" rx="4.2" ry="3.2" fill={NANTY.feature} />
    </>
  );
}

function Thinking() {
  return (
    <>
      <g className="nd-nanty-tilt">
        <path className="nd-nanty-antenna" d="M25 22 C24 13 26 7 33 4" stroke={NANTY.antennaLeft} />
        <circle cx="33" cy="4" r={TIP_R} fill={NANTY.antennaLeft} />
        <path className="nd-nanty-antenna" d="M39 22 C42 14 48 11 56 12" stroke={NANTY.antennaRight} />
        <circle cx="56" cy="12" r={TIP_R} fill={NANTY.antennaRight} />
      </g>
      <Head />
      <Eyes cy={38} shineY={35} shineX={1.6} blink={false} />
      <path className="nd-nanty-line" d="M28 50 L36 49" stroke={NANTY.feature} />
    </>
  );
}

function Sleeping() {
  return (
    <>
      <text className="nd-nanty-z" data-z="1" x="50" y="14" fontSize="9" fontWeight="700" fill={NANTY.sleep} fontFamily="Inter, sans-serif">
        z
      </text>
      <text className="nd-nanty-z" data-z="2" x="54" y="10" fontSize="7" fontWeight="700" fill={NANTY.sleep} fontFamily="Inter, sans-serif">
        z
      </text>
      <g className="nd-nanty-breathe">
        <g className="nd-nanty-droop" data-side="left">
          <path className="nd-nanty-antenna" d="M25 22 C20 18 14 18 9 22" stroke={NANTY.antennaLeft} />
          <circle cx="9" cy="22" r={TIP_R} fill={NANTY.antennaLeft} />
        </g>
        <g className="nd-nanty-droop" data-side="right">
          <path className="nd-nanty-antenna" d="M39 22 C44 18 50 18 55 22" stroke={NANTY.antennaRight} />
          <circle cx="55" cy="22" r={TIP_R} fill={NANTY.antennaRight} />
        </g>
        <Head />
        <path className="nd-nanty-line" d="M21 38 Q25 41 29 38" stroke={NANTY.feature} />
        <path className="nd-nanty-line" d="M35 38 Q39 41 43 38" stroke={NANTY.feature} />
        <path className="nd-nanty-line" data-weight="thin" d="M29 50 Q32 51.5 35 50" stroke={NANTY.feature} />
      </g>
    </>
  );
}

function Waiting() {
  return (
    <>
      <g className="nd-nanty-perk">
        <path className="nd-nanty-antenna" d="M25 22 C23 11 19 5 13 2" stroke={NANTY.antennaLeft} />
        <circle cx="13" cy="2.5" r={TIP_R} fill={NANTY.antennaLeft} />
        <path className="nd-nanty-antenna" d="M39 22 C41 11 45 5 51 2" stroke={NANTY.antennaRight} />
        <circle cx="51" cy="2.5" r={TIP_R} fill={NANTY.antennaRight} />
      </g>
      <Head />
      <Eyes cy={38} rx={4.6} ry={5.8} shineY={35.6} shineR={1.6} shineX={1.6} />
      <path className="nd-nanty-line" d="M28 49 Q32 51 36 49" stroke={NANTY.feature} />
      <circle className="nd-nanty-wave" cx="56" cy="20" r="5.3" fill="none" stroke={NANTY.waiting} />
      <circle className="nd-nanty-dot" cx="56" cy="20" r="5.3" fill={NANTY.waiting} />
    </>
  );
}

const POSES: Readonly<Record<NantyMood, () => JSX.Element>> = {
  attentive: Attentive,
  speaking: Speaking,
  thinking: Thinking,
  sleeping: Sleeping,
  waiting: Waiting,
};

interface NantyAvatarProps {
  mood: NantyMood;
  /** Lado em px. */
  size: number;
  /** Nome acessível (`role="img"`); sem ele, decorativo. */
  label?: string;
  /** Força a pose parada (a foto do bot, testes); sem ele, segue o `prefers-reduced-motion`. */
  still?: boolean;
}

export function NantyAvatar({ mood, size, label, still }: NantyAvatarProps) {
  const reduced = useReducedMotion();
  const Pose = POSES[mood];
  const motion = still === true || reduced ? "still" : "live";
  const a11y = label !== undefined && label !== "" ? { role: "img", "aria-label": label } : { "aria-hidden": true as const };
  return (
    <svg
      className="nd-nanty"
      data-nanty-avatar=""
      data-mood={mood}
      data-motion={motion}
      viewBox="0 0 64 64"
      width={size}
      height={size}
      focusable="false"
      {...a11y}
    >
      <Pose />
    </svg>
  );
}
