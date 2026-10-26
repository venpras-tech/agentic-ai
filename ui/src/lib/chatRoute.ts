export const CHAT_PATH = "/";

export function isChatRoute(pathname: string): boolean {
  return pathname === CHAT_PATH || pathname === "/chat";
}
