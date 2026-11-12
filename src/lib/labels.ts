const STATUS_LABELS: Record<string, string> = {
  active: "تعمل",
  running: "قيد التشغيل",
  in_progress: "قيد التشغيل",
  idle: "خاملة",
  done: "تمّت",
  completed: "مكتملة",
  success: "نجحت",
  interrupted: "متوقفة",
  stopped: "متوقفة",
  cancelled: "ملغاة",
  error: "فشلت",
  failed: "فشلت",
  deleted: "محذوفة",
  pending: "معلّقة",
  connected: "متصل",
  connecting: "يتصل",
  disconnected: "غير متصل",
  answered: "تمت الإجابة",
  timed_out: "انتهت المدة",
};

const ORIGIN_LABELS: Record<string, string> = {
  user: "المستخدم",
  project: "المشروع",
  local: "محلي",
  session: "الجلسة",
  managed: "مُدار",
  always_allow: "سماح دائم",
};

/** Status values are engine identifiers; the screen copy is Arabic. */
export function statusLabel(status: string): string {
  return STATUS_LABELS[status] ?? status;
}

export function originLabel(origin: string): string {
  return ORIGIN_LABELS[origin] ?? origin;
}

/** The host falls back to "(untitled)" before a first prompt arrives. */
export function sessionTitle(title: string | null | undefined): string {
  const value = (title ?? "").trim();
  return !value || value === "(untitled)" ? "بلا عنوان" : value;
}

export function formatCost(micros: number): string {
  const usd = micros / 1_000_000;
  return `$${usd.toFixed(usd < 1 ? 4 : 2)}`;
}
