import type { Settings } from "../lib/types";
import { Switch } from "../components/Switch";
import "./SettingsScreen.css";

export function SettingsScreen({
  settings,
  onChange,
  error,
}: {
  settings: Settings;
  onChange: (key: keyof Settings, value: boolean) => void;
  error: string | null;
}) {
  const rows: { key: keyof Settings; title: string; description: string }[] = [
    {
      key: "auto_connect",
      title: "Автоподключение",
      description: "Подключаться к выбранному профилю при запуске",
    },
    {
      key: "start_with_system",
      title: "Запуск со стартом системы",
      description: "Открывать Riekko при входе в систему",
    },
    {
      key: "notifications",
      title: "Уведомления",
      description: "Сообщать об обрыве и восстановлении соединения",
    },
  ];

  return (
    <div className="screen">
      <div className="screen__header">
        <h1 className="screen__title">Параметры</h1>
      </div>

      {error && <p className="settings-error">{error}</p>}

      <div className="settings-list">
        {rows.map((row) => (
          <div key={row.key} className="settings-row card">
            <div className="settings-row__text">
              <span className="settings-row__title">{row.title}</span>
              <span className="settings-row__description">{row.description}</span>
            </div>
            <Switch
              checked={settings[row.key]}
              label={row.title}
              onChange={(value) => onChange(row.key, value)}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
