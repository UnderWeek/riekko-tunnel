import { Icon } from "./Icon";
import "./Switch.css";

export function Switch({ checked, onChange }: { checked: boolean; onChange: (value: boolean) => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      className={`md-switch${checked ? " md-switch--checked" : ""}`}
      onClick={() => onChange(!checked)}
    >
      <span className="md-switch__thumb">
        {checked && <Icon name="check-circle" size={14} />}
      </span>
    </button>
  );
}
