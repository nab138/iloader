import React, { createContext, useContext, useState } from "react";
import { Modal } from "./components/Modal";
import "./DialogContext.css";
import { useTranslation } from "react-i18next";

export const DialogContext = createContext<{
  confirm: (
    title: string,
    message: string,
    onConfirm: () => void,
    onCancel?: () => void,
  ) => void;
}>({ confirm: () => false });

export const DialogProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const { t } = useTranslation();
  const [title, setTitle] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [onConfirm, setOnConfirm] = useState<(() => void) | null>(null);
  const [onCancel, setOnCancel] = useState<(() => void) | null>(null);

  return (
    <DialogContext.Provider
      value={{
        confirm: (title, message, onConfirm, onCancel) => {
          setTitle(title);
          setMsg(message);
          setOnConfirm(() => onConfirm);
          setOnCancel(() => onCancel ?? null);
          return true;
        },
      }}
    >
      <Modal
        zIndex={99999999}
        isOpen={title !== null || msg !== null}
        close={() => {
          onCancel?.();
          setTitle(null);
          setMsg(null);
          setOnConfirm(null);
          setOnCancel(null);
        }}
      >
        <div className="dialog">
          <h2>{title}</h2>
          <p>{msg}</p>
          <div className="dialog-buttons">
            <button
              className="action-button primary"
              onClick={() => {
                onConfirm?.();
                setTitle(null);
                setMsg(null);
                setOnConfirm(null);
                setOnCancel(null);
              }}
            >
              {t("dialog.confirm")}
            </button>
            <button
              onClick={() => {
                onCancel?.();
                setTitle(null);
                setMsg(null);
                setOnConfirm(null);
                setOnCancel(null);
              }}
            >
              {t("dialog.cancel")}
            </button>
          </div>
        </div>
      </Modal>
      {children}
    </DialogContext.Provider>
  );
};

export const useDialog = () => {
  return useContext(DialogContext);
};
