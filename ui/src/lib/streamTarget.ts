let streamTargetTurnId = "";

export function setStreamTargetTurnId(turnId: string): void {
  streamTargetTurnId = turnId;
}

export function getStreamTargetTurnId(): string {
  return streamTargetTurnId;
}

export function clearStreamTargetTurnId(): void {
  streamTargetTurnId = "";
}
