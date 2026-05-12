import "./Toggle.css";

interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  ariaLabel: string;
}

/// Compact iOS-style capsule switch. Handle on the left = off, handle on
/// the right = on. The on-state fills with the coral accent so the toggle
/// itself doubles as a status indicator (active vs paused).
export function Toggle({ checked, onChange, disabled, ariaLabel }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      className={`pf-toggle ${checked ? "on" : "off"}`}
      onClick={() => onChange(!checked)}
      onKeyDown={(e) => {
        if (e.key === " " || e.key === "Enter") {
          e.preventDefault();
          onChange(!checked);
        }
      }}
    >
      <span className="pf-toggle-handle" aria-hidden />
    </button>
  );
}
