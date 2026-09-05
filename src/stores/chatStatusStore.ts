import { create } from "zustand";
import { initialChatStatus, reduceChatStatus } from "../lib/chatStatus";
import type { ChatStatus, ChatStatusEvent } from "../lib/chatStatus";

interface ChatStatusState {
  status: ChatStatus;
  dispatch: (event: ChatStatusEvent) => void;
}

export const useChatStatusStore = create<ChatStatusState>((set) => ({
  status: initialChatStatus,
  dispatch: (event) =>
    set((s) => ({
      status: reduceChatStatus(s.status, event),
    })),
}));