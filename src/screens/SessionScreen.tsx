import { useEffect, useRef, useState } from "react";
import type { AppState } from "../lib/types";
import { formatMb, formatUptime } from "../lib/format";
import { Button } from "../components/Button";
import { Icon } from "../components/Icon";
import "./SessionScreen.css";

const HISTORY_LENGTH = 14;

export function SessionScreen({
  appState,
  onRefresh,
}: {
  appState: AppState;
  onRefresh: () => void;
}) {
  const { session, state } = appState;
  const isActive = state === "CONNECTED" || state === "RECONNECTING";
  const [history, setHistory] = useState<number[]>(() => Array(HISTORY_LENGTH).fill(0));
  const lastTotal = useRef(session.rx_mb + session.tx_mb);

  useEffect(() => {
    const total = session.rx_mb + session.tx_mb;
    const delta = Math.max(0, total - lastTotal.current);
    lastTotal.current = total;
    setHistory((prev) => [...prev.slice(1), delta]);
  }, [session.rx_mb, session.tx_mb]);

  const max = Math.max(0.05, ...history);

  return (
    <div className="screen">
      <div className="screen__header">
        <h1 className="screen__title">Сессия</h1>
        <Button variant="tonal" icon={<Icon name="refresh" size={18} />} onClick={onRefresh}>
          Обновить
        </Button>
      </div>

      <div className="stat-grid">
        <div className="stat-card">
          <span className="stat-card__label">Endpoint</span>
          <span className="stat-card__value stat-card__value--small">{session.endpoint || "—"}</span>
        </div>
        <div className="stat-card">
          <span className="stat-card__label">Latency</span>
          <span className="stat-card__value">{isActive ? `${session.latency_ms} ms` : "—"}</span>
        </div>
        <div className="stat-card">
          <span className="stat-card__label">Uptime</span>
          <span className="stat-card__value">{formatUptime(session.uptime_secs)}</span>
        </div>
      </div>

      <div className="stat-grid">
        <div className="stat-card">
          <span className="stat-card__label">
            <Icon name="upload" size={14} /> Отправлено
          </span>
          <span className="stat-card__value">{formatMb(session.tx_mb)}</span>
        </div>
        <div className="stat-card">
          <span className="stat-card__label">
            <Icon name="download" size={14} /> Получено
          </span>
          <span className="stat-card__value">{formatMb(session.rx_mb)}</span>
        </div>
      </div>

      <div className="session-chart card">
        <span className="session-chart__label">Трафик, MB / сек</span>
        <div className="session-chart__bars">
          {history.map((value, i) => (
            <div
              key={i}
              className="session-chart__bar"
              style={{ height: `${8 + (value / max) * 92}%` }}
            />
          ))}
        </div>
      </div>
    </div>
  );
}
