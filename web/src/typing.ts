/** Campo de digitação (ou outro controle de texto) tem o foco: não se desvia o teclado dele. */
export function isTypingTarget(element: Element | null): boolean {
  if (element === null || element === document.body) return false;
  const tag = element.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (element as HTMLElement).isContentEditable;
}
