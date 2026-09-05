import type { CSSProperties } from "react";
import type { TunnelState } from "../lib/types";
import "./StatusPill.css";

const LABEL: Record<TunnelState, string> = {
  IDLE: "Отключено",
  STARTING: "Подключение…",
  CONNECTED: "Подключено",
  RECONNECTING: "Восстановление…",
  ERROR: "Ошибка",
};

const TINT: Record<TunnelState, string> = {
  IDLE: "var(--md-on-surface-variant)",
  STARTING: "var(--md-primary)",
  CONNECTED: "var(--md-success)",
  RECONNECTING: "var(--md-primary)",
  ERROR: "var(--md-error)",
};

export function StatusPill({ state }: { state: TunnelState }) {
  return (
    <span className="status-pill" style={{ "--tint": TINT[state] } as CSSProperties}>
      <span className="status-pill__dot" />
      {LABEL[state]}
    </span>
  );
}
