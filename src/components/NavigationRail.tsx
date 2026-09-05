import type { Section } from "../lib/types";
import { Icon, type IconName } from "./Icon";
import "./NavigationRail.css";

const ITEMS: { id: Section; label: string; icon: IconName }[] = [
  { id: "connection", label: "Подключение", icon: "vpn-lock" },
  { id: "profiles", label: "Профили", icon: "folder" },
  { id: "session", label: "Сессия", icon: "monitoring" },
  { id: "settings", label: "Параметры", icon: "tune" },
];

export function NavigationRail({
  section,
  onSelect,
}: {
  section: Section;
  onSelect: (section: Section) => void;
}) {
  return (
    <nav className="nav-rail">
      <div className="nav-rail__brand">❄</div>
      {ITEMS.map((item) => {
        const active = section === item.id;
        return (
          <button
            key={item.id}
            type="button"
            className={`nav-rail__item${active ? " nav-rail__item--active" : ""}`}
            onClick={() => onSelect(item.id)}
            aria-current={active}
          >
            <span className="nav-rail__indicator">
              <Icon name={item.icon} size={22} />
            </span>
            <span className="nav-rail__label">{item.label}</span>
          </button>
        );
      })}
    </nav>
  );
}
