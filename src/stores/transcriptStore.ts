import { create } from "zustand";
import type { LedgerEntry, ChatMessage } from "../types";
import { chatReducer, emptyChat } from "../lib/chatReducer";
import type { ChatAction } from "../lib/chatReducer";

interface TranscriptState {
  messages: ChatMessage[];
  ledger: LedgerEntry[];
  dispatch: (action: ChatAction) => void;
}

export const useTranscriptStore = create<TranscriptState>((set) => ({
  messages: emptyChat.messages,
  ledger: emptyChat.ledger,
  dispatch: (action) =>
    set((s) => {
      const next = chatReducer({ messages: s.messages, ledger: s.ledger }, action);
      return { messages: next.messages, ledger: next.ledger };
    }),
}));