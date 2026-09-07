import { useEffect, useRef, useState } from "react";
import type { AppState } from "../lib/types";
import { formatMb, formatSpeed, formatUptime } from "../lib/format";
import { Button } from "../components/Button";
import { Icon } from "../components/Icon";
import "./SessionScreen.css";

const HISTORY_LENGTH = 20;

interface Tick {
  up: number;
  down: number;
}

export function SessionScreen({
  appState,
  onRefresh,
}: {
  appState: AppState;
  onRefresh: () => void;
}) {
  const { session, state, routing_mode } = appState;
  const isActive = state === "CONNECTED" || state === "RECONNECTING";
  const [history, setHistory] = useState<Tick[]>(() =>
    Array.from({ length: HISTORY_LENGTH }, () => ({ up: 0, down: 0 })),
  );
  const last = useRef({ up: session.tx_mb, down: session.rx_mb });

  useEffect(() => {
    const up = Math.max(0, session.tx_mb - last.current.up);
    const down = Math.max(0, session.rx_mb - last.current.down);
    last.current = { up: session.tx_mb, down: session.rx_mb };
    setHistory((prev) => [...prev.slice(1), { up, down }]);
  }, [session.tx_mb, session.rx_mb]);

  const latest = history[history.length - 1];
  const max = Math.max(0.001, ...history.map((t) => Math.max(t.up, t.down)));
  const hasTrafficSource = routing_mode === "TUN";

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
            <Icon name="upload" size={14} /> Отправлено всего
          </span>
          <span className="stat-card__value">{formatMb(session.tx_mb)}</span>
        </div>
        <div className="stat-card">
          <span className="stat-card__label">
            <Icon name="download" size={14} /> Получено всего
          </span>
          <span className="stat-card__value">{formatMb(session.rx_mb)}</span>
        </div>
      </div>

      <div className="session-chart card">
        <div className="session-chart__header">
          <div className="session-chart__legend">
            <span className="session-chart__legend-item">
              <span className="session-chart__dot session-chart__dot--up" />
              Отправлено — {isActive ? formatSpeed(latest?.up ?? 0) : "—"}
            </span>
            <span className="session-chart__legend-item">
              <span className="session-chart__dot session-chart__dot--down" />
              Получено — {isActive ? formatSpeed(latest?.down ?? 0) : "—"}
            </span>
          </div>
        </div>

        {!hasTrafficSource ? (
          <p className="session-chart__note">
            Живая скорость доступна только в режиме TUN — сейчас подключение идёт через
            системный SOCKS-прокси, у него нет своего счётчика трафика.
          </p>
        ) : (
          <div className="session-chart__bars">
            {history.map((tick, i) => (
              <div key={i} className="session-chart__column">
                <div
                  className="session-chart__bar session-chart__bar--up"
                  style={{ height: `${(tick.up / max) * 100}%` }}
                />
                <div
                  className="session-chart__bar session-chart__bar--down"
                  style={{ height: `${(tick.down / max) * 100}%` }}
                />
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
