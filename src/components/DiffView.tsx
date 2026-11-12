// Line diff for the transcript and S08. The old/new bodies are the engine's
// checkpoint data; the rows here are presentation only (no apply logic).

export type DiffRow = {
  kind: "same" | "add" | "del" | "gap";
  oldLine?: number;
  newLine?: number;
  text: string;
};

export function computeDiff(oldText: string, newText: string): {
  rows: DiffRow[];
  tooLarge: boolean;
  oldCount: number;
  newCount: number;
  added: number;
  deleted: number;
} {
  const a = oldText.split("\n");
  const b = newText.split("\n");
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const midA = a.slice(prefix, a.length - suffix);
  const midB = b.slice(prefix, b.length - suffix);
  const n = midA.length;
  const m = midB.length;
  const tooLarge = n * m > 1_000_000;
  const rows: DiffRow[] = [];

  for (let i = 0; i < prefix; i += 1) {
    rows.push({ kind: "same", oldLine: i + 1, newLine: i + 1, text: a[i] });
  }

  if (!tooLarge) {
    const dp = Array.from({ length: n + 1 }, () => new Int32Array(m + 1));
    for (let i = n - 1; i >= 0; i -= 1) {
      for (let j = m - 1; j >= 0; j -= 1) {
        dp[i][j] =
          midA[i] === midB[j]
            ? dp[i + 1][j + 1] + 1
            : Math.max(dp[i + 1][j], dp[i][j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (midA[i] === midB[j]) {
        rows.push({ kind: "same", oldLine: prefix + i + 1, newLine: prefix + j + 1, text: midA[i] });
        i += 1;
        j += 1;
      } else if (dp[i + 1][j] >= dp[i][j + 1]) {
        rows.push({ kind: "del", oldLine: prefix + i + 1, text: midA[i] });
        i += 1;
      } else {
        rows.push({ kind: "add", newLine: prefix + j + 1, text: midB[j] });
        j += 1;
      }
    }
    while (i < n) {
      rows.push({ kind: "del", oldLine: prefix + i + 1, text: midA[i] });
      i += 1;
    }
    while (j < m) {
      rows.push({ kind: "add", newLine: prefix + j + 1, text: midB[j] });
      j += 1;
    }
  }

  for (let k = 0; k < suffix; k += 1) {
    const ai = a.length - suffix + k;
    const bi = b.length - suffix + k;
    rows.push({ kind: "same", oldLine: ai + 1, newLine: bi + 1, text: a[ai] });
  }

  const added = rows.filter((row) => row.kind === "add").length;
  const deleted = rows.filter((row) => row.kind === "del").length;
  return {
    rows: trimContext(rows),
    tooLarge,
    oldCount: a.length,
    newCount: b.length,
    added,
    deleted,
  };
}

/** A 1000-line unchanged region is not a useful diff; keep changes ±3 lines. */
function trimContext(rows: DiffRow[]): DiffRow[] {
  if (rows.length <= 800) return rows;
  const keep = new Uint8Array(rows.length);
  rows.forEach((row, index) => {
    if (row.kind !== "add" && row.kind !== "del") return;
    const from = Math.max(0, index - 3);
    const to = Math.min(rows.length - 1, index + 3);
    for (let i = from; i <= to; i += 1) keep[i] = 1;
  });
  const out: DiffRow[] = [];
  let gap = false;
  rows.forEach((row, index) => {
    if (keep[index]) {
      out.push(row);
      gap = false;
    } else if (!gap) {
      out.push({ kind: "gap", text: "…" });
      gap = true;
    }
  });
  return out;
}

export function DiffView(props: {
  oldText: string;
  newText: string;
  label?: string;
  dialog?: boolean;
}) {
  const { rows, tooLarge, oldCount, newCount, added, deleted } = computeDiff(
    props.oldText,
    props.newText,
  );

  return (
    <div className="k-diffblock">
      <p className="k-meta" dir="rtl">
        <span>{`${added} سطر مضاف`}</span>
        <span>·</span>
        <span>{`${deleted} سطر محذوف`}</span>
      </p>
      {tooLarge ? (
        <p className="k-note">
          {`الملف أكبر من عرض فرق سطري (${oldCount} → ${newCount} سطر). استخدم استرجاع الملف لإعادته.`}
        </p>
      ) : added === 0 && deleted === 0 ? (
        <p className="k-note">لا فروقات نصية.</p>
      ) : (
        <div
          className={props.dialog ? "k-diffview k-diffview--dialog" : "k-diffview"}
          dir="ltr"
          role="group"
          aria-label={props.label ?? "الفروقات"}
        >
          {rows.map((row, index) => (
            <div key={index} className={`k-diffview__row k-diffview__row--${row.kind}`}>
              <span className="k-diffview__ln">{row.oldLine ?? ""}</span>
              <span className="k-diffview__ln">{row.newLine ?? ""}</span>
              <span className="k-diffview__sign">
                {row.kind === "add" ? "+" : row.kind === "del" ? "-" : row.kind === "gap" ? "…" : "\u00a0"}
              </span>
              <span className="k-diffview__text">{row.text || "\u00a0"}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
