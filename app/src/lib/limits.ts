export const MAX_REPLY_BYTES = 64 * 1024;

export const byteLength = (text: string): number => new TextEncoder().encode(text).length;
