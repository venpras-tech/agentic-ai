import { useCallback, useRef } from "react";

interface ResizeHandleProps {
  /** "x" = vertical bar that adjusts width, "y" = horizontal bar for height. */
  axis: "x" | "y";
  /** Called once when a drag starts (parent snapshots the current size). */
  onDragStart: () => void;
  /** Called per pointermove with the total delta since drag start. */
  onDelta: (delta: number) => void;
}

/**
 * Invisible 4px grab strip between panes. Pointer-capture based so drags
 * survive leaving the element; body text selection is suppressed while
 * dragging so the cursor never flickers into I-beam.
 */
export default function ResizeHandle({
  axis,
  onDragStart,
  onDelta,
}: ResizeHandleProps) {
  const origin = useRef(0);
  const dragging = useRef(false);

  const down = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      dragging.current = true;
      origin.current = axis === "x" ? e.clientX : e.clientY;
      e.currentTarget.setPointerCapture(e.pointerId);
      document.body.style.userSelect = "none";
      onDragStart();
    },
    [axis, onDragStart],
  );

  const move = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!dragging.current) return;
      const pos = axis === "x" ? e.clientX : e.clientY;
      onDelta(pos - origin.current);
    },
    [axis, onDelta],
  );

  const up = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return;
    dragging.current = false;
    e.currentTarget.releasePointerCapture(e.pointerId);
    document.body.style.userSelect = "";
  }, []);

  return (
    <div
      role="separator"
      aria-orientation={axis === "x" ? "vertical" : "horizontal"}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={up}
      className={
        axis === "x"
          ? "z-10 w-1 shrink-0 cursor-col-resize bg-border/60 transition-colors hover:bg-accent/50 active:bg-accent"
          : "z-10 h-1 shrink-0 cursor-row-resize bg-border/60 transition-colors hover:bg-accent/50 active:bg-accent"
      }
    />
  );
}
