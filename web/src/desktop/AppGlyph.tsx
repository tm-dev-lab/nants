/*
 * Ícone de um app onde ele aparece (barra de título, menu do N e alternador do celular). O Nanty
 * (spec 138 T04) aparece pelo avatar colorido, no estado atual que vem da API; os demais, pelo ícone
 * do conjunto. Decorativo: o nome do app fica no texto ao lado.
 */
import { NantyAvatar } from "../apps/nanty/NantyAvatar";
import { nantyMood } from "../apps/nanty/nantyModel";
import { useNanty } from "../apps/nanty/nantyStore";
import { Icon } from "./Icon";
import type { IconSize } from "./theme/tokens";
import { WINDOW_META } from "./windowMeta";
import type { WindowKind } from "./windows";

/** O avatar ocupa a caixa toda do desenho: um pouco maior que o ícone de traço do mesmo lugar. */
const AVATAR_EXTRA = 4;

function LiveNanty({ size }: { size: number }) {
  const { status } = useNanty();
  return <NantyAvatar mood={nantyMood(status)} size={size} />;
}

export function AppGlyph({ kind, size }: { kind: WindowKind; size: IconSize }) {
  if (kind === "nanty") return <LiveNanty size={size + AVATAR_EXTRA} />;
  return <Icon name={WINDOW_META[kind].icon} size={size} />;
}
