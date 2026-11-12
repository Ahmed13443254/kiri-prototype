import { useRef, useState } from "react";
import { Badge, Button } from "./primitives";
import { useFocusTrap } from "../lib/focus";

export interface QuestionOption {
  label?: string;
  description?: string;
  value?: string;
}

export interface Question {
  question: string;
  header?: string;
  multiSelect?: boolean;
  options: QuestionOption[];
}

function optionValue(o: QuestionOption, i: number): string {
  return String(o.value ?? o.label ?? i);
}

export function QuestionCard(props: {
  requestId: string;
  questions: Question[];
  onAnswer: (answers: unknown, response?: string) => void;
}) {
  const { requestId, questions, onAnswer } = props;
  const [picked, setPicked] = useState<Record<number, Set<string>>>({});
  const [freeform, setFreeform] = useState<string>("");
  const [custom, setCustom] = useState<Record<number, string>>({});
  const [answered, setAnswered] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  useFocusTrap(dialogRef);

  if (answered) return null;

  function toggle(qi: number, value: string, multi: boolean) {
    setPicked((prev) => {
      const next = { ...prev };
      const set = new Set(prev[qi] ?? []);
      if (multi) {
        if (set.has(value)) set.delete(value);
        else set.add(value);
      } else {
        set.clear();
        set.add(value);
      }
      next[qi] = set;
      return next;
    });
  }

  function submit() {
    const answers: Record<string, unknown> = {};
    questions.forEach((q, qi) => {
      const set = picked[qi];
      const chosen = set ? Array.from(set) : [];
      const extra = (custom[qi] ?? "").trim();
      const header = q.header ?? `q${qi + 1}`;
      if (q.multiSelect) {
        answers[header] = extra ? [...chosen, extra] : chosen;
      } else if (extra && chosen.length === 0) {
        answers[header] = extra;
      } else {
        answers[header] = chosen[0] ?? extra ?? "";
      }
    });
    setAnswered(true);
    onAnswer(answers, freeform.trim() ? freeform.trim() : undefined);
  }

  const allAnswered = questions.every((_q, qi) => {
    if ((custom[qi] ?? "").trim()) return true;
    const set = picked[qi];
    return Boolean(set && set.size > 0);
  });

  return (
    <div className="k-scrim" role="dialog" aria-modal="true" aria-labelledby={`q-${requestId}`}>
      <div ref={dialogRef} className="k-dialog k-dialog--wide">
        <div className="k-dialog__head">
          <Badge variant="info">سؤال من المحرك</Badge>
        </div>
        <div className="k-questions">
          {questions.map((q, qi) => {
            const legend = q.header ?? `سؤال ${qi + 1}`;
            return (
              <fieldset key={`${requestId}-${qi}`} className="k-fieldset">
                <legend id={qi === 0 ? `q-${requestId}` : undefined}>{legend}</legend>
                <p className="k-question">{q.question}</p>
                <div className="k-options">
                  {q.options.map((o, oi) => {
                    const value = optionValue(o, oi);
                    const checked = Boolean(picked[qi]?.has(value));
                    return (
                      <label
                        key={value}
                        className={`k-option ${checked ? "is-selected" : ""}`}
                        dir="auto"
                      >
                        <input
                          type={q.multiSelect ? "checkbox" : "radio"}
                          name={`q-${requestId}-${qi}`}
                          checked={checked}
                          onChange={() => toggle(qi, value, Boolean(q.multiSelect))}
                        />
                        <span className="k-option__text">
                          <span className="k-option__label">{o.label ?? value}</span>
                          {o.description ? (
                            <span className="k-option__desc">{o.description}</span>
                          ) : null}
                        </span>
                      </label>
                    );
                  })}
                  <label className="k-option">
                    <input
                      type={q.multiSelect ? "checkbox" : "radio"}
                      name={`q-${requestId}-${qi}`}
                      checked={custom[qi] !== undefined}
                      onChange={() =>
                        setCustom((c) => {
                          const next = { ...c };
                          if (q.multiSelect && next[qi] !== undefined) delete next[qi];
                          else next[qi] = next[qi] ?? "";
                          return next;
                        })
                      }
                    />
                    <span className="k-option__text">
                      <span className="k-option__label">إجابة أخرى</span>
                      {custom[qi] !== undefined ? (
                        <input
                          className="k-input"
                          dir="auto"
                          value={custom[qi]}
                          aria-label="إجابة أخرى"
                          onChange={(e) =>
                            setCustom((c) => ({ ...c, [qi]: e.target.value }))
                          }
                        />
                      ) : null}
                    </span>
                  </label>
                </div>
              </fieldset>
            );
          })}

          <label className="k-field">
            <span className="k-label">ردّك (اختياري)</span>
            <textarea
              className="k-input"
              dir="auto"
              value={freeform}
              onChange={(e) => setFreeform(e.target.value)}
              aria-label="ردّ حر"
            />
          </label>
        </div>

        <div className="k-dialog__actions">
          <Button variant="primary" onClick={submit} disabled={!allAnswered}>
            إرسال
          </Button>
        </div>
      </div>
    </div>
  );
}
