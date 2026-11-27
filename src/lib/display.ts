const INVISIBLE_REF_CHARACTERS = /[\u061C\u200B-\u200F\u202A-\u202E\u2066-\u2069\uFEFF]/gu;

export function safeRefLabel(name: string): string {
  return name.replace(INVISIBLE_REF_CHARACTERS, character =>
    `⟨U+${character.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}⟩`,
  );
}
