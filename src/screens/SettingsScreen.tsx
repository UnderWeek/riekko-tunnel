import type { Settings } from "../lib/types";
import { Switch } from "../components/Switch";
import "./SettingsScreen.css";

export function SettingsScreen({
  settings,
  onChange,
}: {
  settings: Settings;
  onChange: (key: keyof Settings, value: boolean) => void;
}) {
  const rows: { key: keyof Settings; title: string; description: string }[] = [
    {
      key: "auto_connect",
      title: "Автоподключение",
      description: "Подключаться к последнему профилю при запуске",
    },
    {
      key: "start_with_system",
      title: "Запуск со стартом системы",
      description: "Открывать Riekko при входе в Windows",
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

      <div className="settings-list">
        {rows.map((row) => (
          <div key={row.key} className="settings-row card">
            <div className="settings-row__text">
              <span className="settings-row__title">{row.title}</span>
              <span className="settings-row__description">{row.description}</span>
            </div>
            <Switch checked={settings[row.key]} onChange={(value) => onChange(row.key, value)} />
          </div>
        ))}
      </div>
    </div>
  );
}
