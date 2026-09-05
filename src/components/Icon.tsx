import vpnLock from "../assets/icons/vpn_lock.svg?raw";
import folder from "../assets/icons/folder.svg?raw";
import monitoring from "../assets/icons/monitoring.svg?raw";
import tune from "../assets/icons/tune.svg?raw";
import powerSettingsNew from "../assets/icons/power_settings_new.svg?raw";
import add from "../assets/icons/add.svg?raw";
import deleteIcon from "../assets/icons/delete.svg?raw";
import refresh from "../assets/icons/refresh.svg?raw";
import wifiTethering from "../assets/icons/wifi_tethering.svg?raw";
import speed from "../assets/icons/speed.svg?raw";
import timer from "../assets/icons/timer.svg?raw";
import upload from "../assets/icons/upload.svg?raw";
import download from "../assets/icons/download.svg?raw";
import close from "../assets/icons/close.svg?raw";
import checkCircle from "../assets/icons/check_circle.svg?raw";
import errorIcon from "../assets/icons/error.svg?raw";
import sync from "../assets/icons/sync.svg?raw";
import dns from "../assets/icons/dns.svg?raw";
import lan from "../assets/icons/lan.svg?raw";
import shieldLock from "../assets/icons/shield_lock.svg?raw";
import link from "../assets/icons/link.svg?raw";
import contentPaste from "../assets/icons/content_paste.svg?raw";
import bolt from "../assets/icons/bolt.svg?raw";
import priorityHigh from "../assets/icons/priority_high.svg?raw";
import expandMore from "../assets/icons/expand_more.svg?raw";
import dragIndicator from "../assets/icons/drag_indicator.svg?raw";
import createNewFolder from "../assets/icons/create_new_folder.svg?raw";
import edit from "../assets/icons/edit.svg?raw";
import cloudDownload from "../assets/icons/cloud_download.svg?raw";

const icons = {
  "vpn-lock": vpnLock,
  folder,
  monitoring,
  tune,
  power: powerSettingsNew,
  add,
  delete: deleteIcon,
  refresh,
  "wifi-tethering": wifiTethering,
  speed,
  timer,
  upload,
  download,
  close,
  "check-circle": checkCircle,
  error: errorIcon,
  sync,
  dns,
  lan,
  "shield-lock": shieldLock,
  link,
  "content-paste": contentPaste,
  bolt,
  "priority-high": priorityHigh,
  "expand-more": expandMore,
  "drag-indicator": dragIndicator,
  "create-new-folder": createNewFolder,
  edit,
  "cloud-download": cloudDownload,
} satisfies Record<string, string>;

export type IconName = keyof typeof icons;

export function Icon({
  name,
  size = 22,
  className,
}: {
  name: IconName;
  size?: number;
  className?: string;
}) {
  return (
    <span
      className={`icon${className ? ` ${className}` : ""}`}
      style={{ width: size, height: size }}
      dangerouslySetInnerHTML={{ __html: icons[name] }}
    />
  );
}
