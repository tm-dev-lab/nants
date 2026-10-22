/*
 * Regras puras das imagens do usuário na conversa com o Arquiteto (spec 082 T05), fora do cliente
 * HTTP (que os testes de componente costumam simular).
 */

/** Imagens por mensagem ao Arquiteto. */
export const MAX_CHAT_ATTACHMENTS = 4;

/** Nome de anexo gerado pelo servidor: 32 hex minúsculos + `.png`, `.jpg` ou `.webp`. */
const ATTACHMENT_NAME_PATTERN = /^[0-9a-f]{32}\.(png|jpg|webp)$/;

export function isAttachmentName(name: string): boolean {
  return ATTACHMENT_NAME_PATTERN.test(name);
}

/** URL de `GET /api/projects/:id/chat/attachments/:name`. */
export function chatAttachmentUrl(projectId: string, name: string): string {
  return `/api/projects/${encodeURIComponent(projectId)}/chat/attachments/${encodeURIComponent(name)}`;
}
