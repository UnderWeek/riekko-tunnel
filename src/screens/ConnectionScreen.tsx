import type { AppState } from "../lib/types";
import { formatMb, formatUptime } from "../lib/format";
import { StatusPill } from "../components/StatusPill";
import { Icon } from "../components/Icon";
import "./ConnectionScreen.css";

export function ConnectionScreen({
  appState,
  onToggleConnection,
  error,
}: {
  appState: AppState;
  onToggleConnection: () => void;
  error: string | null;
}) {
  const { state, routing_mode, session, profiles, active_profile_id } = appState;
  const isConnected = state === "CONNECTED" || state === "RECONNECTING";
  const activeProfile = profiles.find((p) => p.id === active_profile_id);

  return (
    <div className="screen connection-screen">
      <div className="connection-screen__header">
        <h1 className="connection-screen__title">RIEKKO</h1>
        <div className="connection-screen__status-group">
          {isConnected && routing_mode && (
            <span
              className={`routing-badge${routing_mode === "TUN" ? " routing-badge--tun" : " routing-badge--proxy"}`}
              title={
                routing_mode === "TUN"
                  ? "Весь системный трафик идёт через туннель"
                  : "TUN недоступен — через туннель идут только приложения, использующие системный прокси"
              }
            >
              {routing_mode === "TUN" ? "TUN · весь трафик" : "SOCKS-прокси"}
            </span>
          )}
          <StatusPill state={state} />
        </div>
      </div>

      {activeProfile ? (
        <p className="connection-screen__profile-name">{activeProfile.name}</p>
      ) : (
        <p className="connection-screen__profile-name connection-screen__profile-name--muted">
          Профиль не выбран — откройте «Профили»
        </p>
      )}

      <div className="connection-card">
        <div className="connection-card__row">
          <span>Endpoint</span>
          <strong>{session.endpoint || "—"}</strong>
        </div>
        <div className="connection-card__row">
          <span>Latency</span>
          <strong>{isConnected && session.latency_ms > 0 ? `${session.latency_ms} ms` : "—"}</strong>
        </div>
        <div className="connection-card__row">
          <span>Uptime</span>
          <strong>{formatUptime(session.uptime_secs)}</strong>
        </div>

        <div className="connection-card__divider" />

        <div className="connection-card__row">
          <span>
            <Icon name="upload" size={16} /> Отправлено
          </span>
          <strong>{formatMb(session.tx_mb)}</strong>
        </div>
        <div className="connection-card__row">
          <span>
            <Icon name="download" size={16} /> Получено
          </span>
          <strong>{formatMb(session.rx_mb)}</strong>
        </div>
      </div>

      <div className="connection-screen__spacer" />

      {error && (
        <p className="connection-error">
          <Icon name="priority-high" size={14} />
          {error}
        </p>
      )}

      <button
        type="button"
        className={`connect-button${isConnected ? " connect-button--active" : ""}`}
        onClick={onToggleConnection}
        disabled={!activeProfile && !isConnected}
      >
        <span className="connect-button__icon">
          <Icon name="power" size={20} />
        </span>
        {isConnected ? "Отключить" : "Подключить"}
      </button>
    </div>
  );
}
