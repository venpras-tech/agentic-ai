import { useEffect, useState } from "react";

export function useDebouncedResize(delayMs = 80) {
  const [size, setSize] = useState<{ width: number; height: number }>(() => ({
    width: window.innerWidth,
    height: window.innerHeight,
  }));

  useEffect(() => {
    let timer: number | undefined;
    const onResize = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        setSize({ width: window.innerWidth, height: window.innerHeight });
      }, delayMs);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("resize", onResize);
    };
  }, [delayMs]);

  return size;
}
