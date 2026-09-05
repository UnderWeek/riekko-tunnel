import type { MouseEvent, ReactNode } from "react";
import "./Button.css";

type Variant = "filled" | "tonal" | "text" | "icon";

export function Button({
  variant = "filled",
  icon,
  children,
  onClick,
  disabled,
  danger,
  title,
}: {
  variant?: Variant;
  icon?: ReactNode;
  children?: ReactNode;
  onClick?: (event: MouseEvent<HTMLButtonElement>) => void;
  disabled?: boolean;
  danger?: boolean;
  title?: string;
}) {
  return (
    <button
      type="button"
      title={title}
      className={`md-button md-button--${variant}${danger ? " md-button--danger" : ""}`}
      onClick={onClick}
      disabled={disabled}
    >
      {icon}
      {children !== undefined && <span>{children}</span>}
    </button>
  );
}
