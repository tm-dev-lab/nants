/*
 * Marca do produto (spec 093, "Marca"; spec 100, decisão 1): o nome é "Nants" e o slogan,
 * escolhido pelo usuário, vive só aqui. Toda a interface lê estas constantes.
 * Regra de identidade: o slogan vem SEMPRE logo depois da logo (símbolo e nome), sem nenhum texto
 * entre os dois; a linha de produto (`BRAND_PRODUCT_LINE_KEY`), quando aparece, vem depois do slogan.
 */
import type { MessageKey } from "../i18n";

export const BRAND_NAME = "Nants";

export const BRAND_SLOGAN = "Software built by agents. Steered by you.";

/**
 * Linha de produto dos textos de apresentação (boas-vindas), sempre depois do slogan. Vem do
 * dicionário (spec 101): o nome e o slogan são iguais nos dois idiomas; a linha de produto, não.
 */
export const BRAND_PRODUCT_LINE_KEY = "brand.productLine" satisfies MessageKey;
