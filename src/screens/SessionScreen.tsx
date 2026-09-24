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
  const last = useRef({ up: session.tx_mb, down: session.rx_mb, uptime: session.uptime_secs });

  // One sample per elapsed second of uptime — keyed on the clock, not on
  // the counters, or an idle link would never record its zero-speed
  // seconds and the chart would freeze on the last burst.
  useEffect(() => {
    const prev = last.current;
    const seconds = session.uptime_secs - prev.uptime;
    // Same second (two ticks close together): keep the old baseline so
    // the bytes still count towards the next sample.
    if (seconds === 0) return;
    last.current = { up: session.tx_mb, down: session.rx_mb, uptime: session.uptime_secs };
    if (seconds < 0) return; // a new session started
    const up = Math.max(0, session.tx_mb - prev.up) / seconds;
    const down = Math.max(0, session.rx_mb - prev.down) / seconds;
    setHistory((h) => [...h.slice(1), { up, down }]);
  }, [session.uptime_secs, session.tx_mb, session.rx_mb]);

  // A new session (or a disconnect) starts the chart from scratch.
  useEffect(() => {
    if (session.uptime_secs === 0) {
      setHistory(Array.from({ length: HISTORY_LENGTH }, () => ({ up: 0, down: 0 })));
    }
  }, [session.uptime_secs]);

  const latest = history[history.length - 1];
  const max = Math.max(0.001, ...history.map((t) => Math.max(t.up, t.down)));
  const hasTrafficSource = routing_mode === "TUN";
  const showTotals = isActive && hasTrafficSource;

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
          <span className="stat-card__value stat-card__value--small" title={session.endpoint || undefined}>
            {session.endpoint || "—"}
          </span>
        </div>
        <div className="stat-card">
          <span className="stat-card__label">Latency</span>
          <span className="stat-card__value">
            {isActive && session.latency_ms > 0 ? `${session.latency_ms} ms` : "—"}
          </span>
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
          <span className="stat-card__value">{showTotals ? formatMb(session.tx_mb) : "—"}</span>
        </div>
        <div className="stat-card">
          <span className="stat-card__label">
            <Icon name="download" size={14} /> Получено всего
          </span>
          <span className="stat-card__value">{showTotals ? formatMb(session.rx_mb) : "—"}</span>
        </div>
      </div>

      <div className="session-chart card">
        <div className="session-chart__header">
          <div className="session-chart__legend">
            <span className="session-chart__legend-item">
              <span className="session-chart__dot session-chart__dot--up" />
              Отправлено — {showTotals ? formatSpeed(latest?.up ?? 0) : "—"}
            </span>
            <span className="session-chart__legend-item">
              <span className="session-chart__dot session-chart__dot--down" />
              Получено — {showTotals ? formatSpeed(latest?.down ?? 0) : "—"}
            </span>
          </div>
        </div>

        {!isActive ? (
          <p className="session-chart__note">Нет активного подключения.</p>
        ) : !hasTrafficSource ? (
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
