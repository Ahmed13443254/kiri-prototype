import type { ReactNode } from "react";

/** One primitive, one spelling. No component writes a hex. */
export function Button(props: {
  variant?: "primary" | "secondary" | "ghost" | "danger" | "danger-ghost";
  size?: "sm" | "md" | "lg";
  busy?: boolean;
  children: ReactNode;
  className?: string;
  onClick?: (e: React.MouseEvent<HTMLButtonElement>) => void;
  disabled?: boolean;
  type?: "button" | "submit";
  title?: string;
}) {
  const variant = props.variant ?? "secondary";
  const size = props.size ?? "md";
  return (
    <button
      type={props.type ?? "button"}
      onClick={(event) => {
        if (!props.disabled) props.onClick?.(event);
      }}
      title={props.title}
      aria-busy={props.busy || undefined}
      disabled={props.disabled || props.busy || undefined}
      aria-disabled={props.disabled || props.busy || undefined}
      className={[
        "k-btn",
        `k-btn--${variant}`,
        `k-btn--${size}`,
        props.busy ? "is-busy" : "",
        props.className ?? "",
      ].join(" ")}
    >
      {props.busy ? <span className="k-spin" aria-hidden="true" /> : null}
      {props.children}
    </button>
  );
}

export function IconButton(props: {
  label: string;
  children: ReactNode;
  onClick?: () => void;
  pressed?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={props.label}
      aria-pressed={props.pressed}
      aria-disabled={props.disabled || undefined}
      onClick={() => {
        if (!props.disabled) props.onClick?.();
      }}
      className="k-iconbtn"
      title={props.label}
    >
      {props.children}
    </button>
  );
}

export function Badge(props: { variant?: string; children: ReactNode; ltr?: boolean }) {
  return (
    <span className={`k-badge ${props.variant ? `k-badge--${props.variant}` : ""}`} {...(props.ltr ? { dir: "ltr" } : {})}>
      {props.children}
    </span>
  );
}

export function EmptyState(props: {
  title: string;
  body: string;
  action?: ReactNode;
}) {
  return (
    <div className="k-empty">
      <svg width="72" height="72" viewBox="0 0 72 72" fill="none" aria-hidden="true">
        <rect x="10" y="18" width="52" height="36" rx="8" stroke="var(--k-border-strong)" strokeWidth="2" />
        <path d="M24 33h24M24 41h16" stroke="var(--k-border-strong)" strokeWidth="2" strokeLinecap="round" />
      </svg>
      <h2>{props.title}</h2>
      <p>{props.body}</p>
      {props.action}
    </div>
  );
}

export function Mono(props: { children: ReactNode }) {
  return (
    <bdi dir="ltr" className="k-mono">
      {props.children}
    </bdi>
  );
}
