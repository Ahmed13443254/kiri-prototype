const MODELS = [
  { id: "claude-opus-5-5", label: "Claude Opus 5.5", detail: "الأقوى للمهام المعقدة" },
  { id: "claude-opus-5", label: "Claude Opus 5", detail: "أداء عالٍ للبرمجة" },
  { id: "claude-sonnet-5-5", label: "Claude Sonnet 5.5", detail: "متوازن وسريع" },
  { id: "claude-haiku-5-5", label: "Claude Haiku 5.5", detail: "الأسرع والأقل تكلفة" },
];

export function ModelSelect(props: { value: string; changed: boolean; onChange: (model: string) => void }) {
  const selected = MODELS.find((model) => model.id === props.value);
  return (
    <label className={`k-modelselect ${props.changed ? "is-changed" : ""}`}>
      <span className="k-modelselect__label">النموذج</span>
      <span className="k-modelselect__control">
        <select
          value={props.value}
          aria-label="النموذج"
          title={selected?.detail ?? props.value}
          onChange={(event) => props.onChange(event.target.value)}
        >
          {!selected ? <option value={props.value}>{props.value}</option> : null}
          {MODELS.map((model) => <option key={model.id} value={model.id}>{model.label}</option>)}
        </select>
        <svg viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="m4 6 4 4 4-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
      </span>
    </label>
  );
}
