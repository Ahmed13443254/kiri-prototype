import { useEffect, type RefObject } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]):not([aria-disabled="true"]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Keep Tab and Shift+Tab inside a modal for as long as it is open. */
export function useFocusTrap(ref: RefObject<HTMLElement>, active = true) {
  useEffect(() => {
    if (!active) return;
    const node = ref.current;
    if (!node) return;

    function onKey(event: KeyboardEvent) {
      if (event.key !== "Tab") return;
      const items = Array.from(node!.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
        (el) => el.offsetParent !== null,
      );
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      const current = document.activeElement as HTMLElement | null;
      if (event.shiftKey) {
        if (!current || current === first || !node!.contains(current)) {
          event.preventDefault();
          last.focus();
        }
      } else if (!current || current === last || !node!.contains(current)) {
        event.preventDefault();
        first.focus();
      }
    }

    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [ref, active]);
}
