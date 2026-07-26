import "./Dropdown.css";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

export type DropdownOption = {
  value: string;
  label: string;
  subLabel?: string;
  detail?: string;
};

type DropdownProps = {
  label: string;
  labelId: string;
  options: DropdownOption[];
  value: string;
  onChange: (value: string) => void;
  allowCustom?: boolean;
  defaultCustomValue?: string;
  customPlaceholder?: string;
  customLabel?: string;
  customToggleLabel?: string;
  presetToggleLabel?: string;
};

const customValueKey = "__custom__";

export const Dropdown = ({
  label,
  labelId,
  options,
  value,
  onChange,
  allowCustom = false,
  defaultCustomValue = "",
  customPlaceholder,
  customLabel,
  customToggleLabel,
  presetToggleLabel,
}: DropdownProps) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [flipUp, setFlipUp] = useState(false);
  const dropdownRef = useRef<HTMLDivElement | null>(null);
  const toggleRef = useRef<HTMLButtonElement | null>(null);
  const frozenOptionsRef = useRef(options);
  const customInputRef = useRef<HTMLInputElement | null>(null);
  const lastCustomValueRef = useRef<string>(
    allowCustom && options.every((option) => option.value !== value)
      ? value
      : defaultCustomValue,
  );

  const isCustom = useMemo(() => {
    if (!allowCustom) return false;
    return options.every((option) => option.value !== value);
  }, [allowCustom, options, value]);

  const openMenu = () => {
    frozenOptionsRef.current = options;
    setOpen(true);
  };

  const closeMenu = () => {
    setOpen(false);
  };

  useEffect(() => {
    if (!open || !toggleRef.current) return;
    const rect = toggleRef.current.getBoundingClientRect();
    const spaceBelow = window.innerHeight - rect.bottom;
    const menuMaxHeight = 260 + 14;
    setFlipUp(spaceBelow < menuMaxHeight && rect.top > spaceBelow);
  }, [open]);

  useEffect(() => {
    if (!open) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(event.target as Node)
      ) {
        closeMenu();
      }
    };

    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        closeMenu();
      }
    };

    document.addEventListener("click", handleClickOutside);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("click", handleClickOutside);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [open]);

  useEffect(() => {
    if (!allowCustom) return;
    if (isCustom) {
      lastCustomValueRef.current = value;
      customInputRef.current?.focus();
    }
  }, [allowCustom, isCustom, value]);

  const activateCustom = () => {
    closeMenu();
    const nextValue = lastCustomValueRef.current || defaultCustomValue;
    if (nextValue) {
      onChange(nextValue);
    }
  };

  const deactivateCustom = () => {
    closeMenu();
    if (options.length > 0) {
      onChange(options[0].value);
    }
  };

  const resolvedCustomPlaceholder =
    customPlaceholder || t("dropdown.custom_value");
  const resolvedCustomLabel = customLabel || t("dropdown.custom");
  const resolvedCustomToggleLabel =
    customToggleLabel || t("dropdown.use_custom_value");
  const resolvedPresetToggleLabel =
    presetToggleLabel || t("dropdown.back_preset_options");

  const menuLabel =
    lastCustomValueRef.current && lastCustomValueRef.current.length > 0
      ? `${resolvedCustomLabel} (${lastCustomValueRef.current})`
      : resolvedCustomLabel;
  const menuOptions = open ? frozenOptionsRef.current : options;
  const dropdownOptions = allowCustom
    ? [...menuOptions, { value: customValueKey, label: menuLabel }]
    : menuOptions;
  const selectedValue = isCustom ? customValueKey : value;
  const presetLabel =
    options.find((option) => option.value === value)?.label ??
    t("dropdown.select");
  const presetSubLabel = options.find((option) => option.value === value)
    ?.subLabel;
  const selectedLabel = isCustom
    ? lastCustomValueRef.current || resolvedCustomLabel
    : presetLabel;

  const selectOption = (optionValue: string) => {
    closeMenu();
    onChange(optionValue);
  };

  return (
    <div>
      <label className="settings-label has-dropdown" id={labelId}>
        <span>{label}</span>
        <div
          className={`custom-dropdown${open ? " open" : ""}${flipUp ? " flip-up" : ""}`}
          ref={dropdownRef}
        >
          <button
            type="button"
            className="dropdown-toggle"
            ref={toggleRef}
            aria-haspopup="listbox"
            aria-expanded={open}
            aria-labelledby={labelId}
            onClick={() => (open ? closeMenu() : openMenu())}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown" || event.key === "Enter") {
                event.preventDefault();
                openMenu();
              }
            }}
          >
            <span className="dropdown-selected-text">
              <span>{selectedLabel}</span>
              {!isCustom && presetSubLabel ? (
                <span className="dropdown-sub-label">{presetSubLabel}</span>
              ) : null}
            </span>
            <span className="dropdown-caret" aria-hidden="true" />
          </button>
          {open && (
            <div
              className="dropdown-menu"
              role="listbox"
              aria-labelledby={labelId}
            >
              {dropdownOptions.map((option) => {
                const isSelected = selectedValue === option.value;
                return (
                  <button
                    key={option.value}
                    type="button"
                    role="option"
                    className={`dropdown-option${isSelected ? " selected" : ""}`}
                    aria-selected={isSelected}
                    onMouseDown={(event) => {
                      event.preventDefault();
                    }}
                    onClick={() => {
                      if (option.value === customValueKey) {
                        activateCustom();
                      } else {
                        selectOption(option.value);
                      }
                    }}
                  >
                    <span className="dropdown-option-text">
                      <span className="dropdown-option-main">
                        <span>{option.label}</span>
                        {option.subLabel ? (
                          <span className="dropdown-sub-label">
                            {option.subLabel}
                          </span>
                        ) : null}
                      </span>
                      {option.detail ? (
                        <span className="dropdown-detail">
                          {option.detail}
                        </span>
                      ) : null}
                    </span>
                    {isSelected && (
                      <span className="checkmark" aria-hidden="true">
                        ✓
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </label>
      {allowCustom && !isCustom ? (
        <div className="custom-toggle">
          <button
            type="button"
            className="link-button"
            onClick={activateCustom}
          >
            {resolvedCustomToggleLabel}
          </button>
        </div>
      ) : null}
      {allowCustom && isCustom ? (
        <div className="custom-toggle">
          <button
            type="button"
            className="link-button muted"
            onClick={deactivateCustom}
          >
            {resolvedPresetToggleLabel}
          </button>
        </div>
      ) : null}
      {allowCustom && isCustom ? (
        <input
          className="settings-label custom-anisette"
          type="text"
          placeholder={resolvedCustomPlaceholder}
          value={value}
          onChange={(event) => {
            onChange(event.target.value);
          }}
          ref={customInputRef}
        />
      ) : null}
    </div>
  );
};
