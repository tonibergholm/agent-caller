export const MAX_REPLY_BYTES = 64 * 1024;

export const byteLength = (text: string): number => new TextEncoder().encode(text).length;

/** Typed answers: acd's `ANSWER_TEXT_MAX_BYTES`. */
export const MAX_ANSWER_BYTES = 16 * 1024;
